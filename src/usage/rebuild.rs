//! Transactional usage epoch rebuild protocol.
//!
//! This module owns build creation, manifest membership freezing, selective
//! replacement, present-source progress, completion CAS, and epoch activation.
//! Persistent missing-source carry batches are committed by `storage::usage`
//! against the same durable manifest.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

type OldBuildProof = (
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    u64,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    u64,
    u64,
    String,
    Option<u64>,
);
type BuildMemberRow = (
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    u64,
    u64,
    String,
    Option<u64>,
    String,
    String,
    u64,
    Option<Vec<u8>>,
    i64,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStatus {
    Pending,
    Rebuilt,
    Carried,
    Blocked,
    Quarantined,
}

impl CompletionStatus {
    fn parse(value: &str) -> Result<Self, RebuildError> {
        match value {
            "pending" => Ok(Self::Pending),
            "rebuilt" => Ok(Self::Rebuilt),
            "carried" => Ok(Self::Carried),
            "blocked" => Ok(Self::Blocked),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(RebuildError::Invalid("unknown completion status")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    pub source_file_id: i64,
    pub expected_generation: i64,
    pub expected_device_id: i64,
    pub expected_inode: i64,
    pub required_through_offset: u64,
    pub observed_raw_size: u64,
    pub membership_reason: String,
    pub completion_status: CompletionStatus,
    pub completion_error_code: Option<String>,
    pub completed_through_offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildSnapshot {
    pub active_epoch: i64,
    pub build_epoch: i64,
    pub target_parser_version: i64,
    pub members: Vec<ManifestEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailProof {
    Unverified,
    None,
    HalfLine { start_offset: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceProgress {
    pub source_file_id: i64,
    pub expected_generation: i64,
    pub start_offset: u64,
    pub last_complete_offset: u64,
    pub observed_raw_size: u64,
    pub expected_guard_hash: Option<Vec<u8>>,
    pub guard_hash: Option<Vec<u8>>,
    pub tail: TailProof,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressOutcome {
    Advanced,
    Rebuilt,
    AlreadyApplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivationOutcome {
    pub active_epoch: i64,
    pub data_revision: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveQuarantineState {
    pub unchanged_source_ids: Vec<i64>,
    pub dirty: bool,
}

pub struct RebuildLedger<'connection> {
    connection: &'connection mut Connection,
}

impl<'connection> RebuildLedger<'connection> {
    pub fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }

    /// Start a fresh build, resume an identical build, or replace a build whose
    /// target parser changed. `present_source_ids` is the complete fixed-view
    /// set for the round; active contributors are unioned in SQL.
    pub fn begin_or_resume(
        &mut self,
        target_parser_version: i64,
        present_source_ids: &[i64],
        now_ms: i64,
    ) -> Result<BuildSnapshot, RebuildError> {
        if target_parser_version < 0 || now_ms < 0 {
            return Err(RebuildError::Invalid("negative parser version or time"));
        }
        if crate::usage::canonical_algorithm_for(target_parser_version).is_none() {
            return Err(RebuildError::Invalid(
                "unknown usage parser canonical algorithm",
            ));
        }
        let present = normalized_ids(present_source_ids)?;
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(begin_or_resume)),
            self.connection,
            None,
        )?;
        let snapshot = source_tx.apply_codex_rebuild_begin_or_resume(
            target_parser_version,
            &present,
            now_ms,
        )?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))?;
        Ok(snapshot)
    }

    /// Commit one present-source reader boundary. The first successful batch
    /// changes the usage checkpoint from rebuild-required/0 to ready; later
    /// batches resume exactly from the committed nonzero offset.
    pub fn record_progress(
        &mut self,
        progress: SourceProgress,
    ) -> Result<ProgressOutcome, RebuildError> {
        validate_progress_shape(&progress)?;
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(record_progress)),
            self.connection,
            None,
        )?;
        let outcome = source_tx.apply_codex_rebuild_record_progress(&progress)?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))?;
        Ok(outcome)
    }

    pub fn block_source(
        &mut self,
        source_file_id: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<(), RebuildError> {
        if error_code.is_empty() || now_ms < 0 {
            return Err(RebuildError::Invalid("invalid block details"));
        }
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(block_source)),
            self.connection,
            None,
        )?;
        source_tx.apply_codex_rebuild_block_source(source_file_id, error_code, now_ms)?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))
    }

    /// Remove every build contribution for one Session Tree and mark all of its
    /// manifest members as quarantined. The build can still activate, but the
    /// quarantined root contributes no usage rows to that epoch.
    pub fn quarantine_session(
        &mut self,
        root_session_id: &str,
        error_code: &str,
        now_ms: i64,
    ) -> Result<usize, RebuildError> {
        if root_session_id.is_empty() || error_code.is_empty() || now_ms < 0 {
            return Err(RebuildError::Invalid("invalid session quarantine details"));
        }
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(quarantine_session)),
            self.connection,
            None,
        )?;
        let count = source_tx.apply_codex_rebuild_quarantine_session(
            root_session_id,
            error_code,
            now_ms,
        )?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))?;
        Ok(count)
    }

    /// Return the unchanged active quarantine source IDs and whether at least
    /// one quarantined Session Tree has changed and therefore needs a shadow retry.
    pub fn active_quarantine_state(&mut self) -> Result<ActiveQuarantineState, RebuildError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let active_epoch: i64 = transaction.query_row(
            "SELECT active_epoch FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| row.get(0),
        )?;
        if active_epoch == 0 {
            transaction.commit()?;
            return Ok(ActiveQuarantineState::default());
        }
        let mut roots_statement = transaction.prepare(
            "SELECT root_session_id FROM usage_session_quarantine
             WHERE ledger_epoch=?1 ORDER BY root_session_id",
        )?;
        let roots = roots_statement
            .query_map([active_epoch], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(roots_statement);

        let mut state = ActiveQuarantineState::default();
        for root in roots {
            let mut proof_statement = transaction.prepare(
                "SELECT source_file_id,file_generation,device_id,inode,observed_size
                 FROM usage_session_quarantine_sources
                 WHERE ledger_epoch=?1 AND root_session_id=?2 ORDER BY source_file_id",
            )?;
            let proofs = proof_statement
                .query_map(params![active_epoch, root], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, u64>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(proof_statement);
            let proof_ids = proofs.iter().map(|proof| proof.0).collect::<BTreeSet<_>>();
            let present_ids = query_ids_string(
                &transaction,
                "SELECT sf.source_file_id
                 FROM source_files sf JOIN threads t ON t.thread_id=sf.thread_id
                 WHERE sf.file_status='present' AND t.root_session_id=?1
                 ORDER BY sf.source_file_id",
                &root,
            )?
            .into_iter()
            .collect::<BTreeSet<_>>();
            let mut clean = proof_ids == present_ids && !proofs.is_empty();
            if clean {
                for (source_id, generation, device_id, inode, observed_size) in &proofs {
                    let matches: i64 = transaction.query_row(
                        "SELECT count(*) FROM source_files
                         WHERE source_file_id=?1 AND file_status='present'
                           AND file_generation=?2 AND device_id=?3 AND inode=?4 AND observed_size=?5",
                        params![source_id, generation, device_id, inode, observed_size],
                        |row| row.get(0),
                    )?;
                    if matches != 1 {
                        clean = false;
                        break;
                    }
                }
            }
            if clean {
                state.unchanged_source_ids.extend(proof_ids);
            } else {
                state.dirty = true;
            }
        }
        state.unchanged_source_ids.sort_unstable();
        state.unchanged_source_ids.dedup();
        transaction.commit()?;
        Ok(state)
    }

    pub fn retry_blocked(&mut self, source_file_id: i64, now_ms: i64) -> Result<(), RebuildError> {
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(retry_blocked)),
            self.connection,
            None,
        )?;
        source_tx.apply_codex_rebuild_retry_blocked(source_file_id, now_ms)?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))
    }

    /// Activate only after a complete discovery proof and every manifest row
    /// has a current completion proof. Old epoch facts are intentionally kept.
    pub fn activate(
        &mut self,
        complete_present_source_ids: &[i64],
    ) -> Result<ActivationOutcome, RebuildError> {
        let present = normalized_ids(complete_present_source_ids)?;
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            concat!("legacy-codex-rebuild:", stringify!(activate)),
            self.connection,
            None,
        )?;
        let outcome = source_tx.apply_codex_rebuild_activate(&present)?;
        source_tx
            .commit()
            .map_err(|_| RebuildError::Invalid("source write transaction commit failed"))?;
        Ok(outcome)
    }
}

pub(crate) fn apply_codex_rebuild_begin_or_resume(
    transaction: &Connection,
    target_parser_version: i64,
    present: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<BuildSnapshot, RebuildError> {
    verify_present_ids(transaction, present)?;
    let (active_epoch, existing_build, existing_target): (i64, Option<i64>, Option<i64>) =
        transaction.query_row(
            "SELECT active_epoch, build_epoch, build_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let build_epoch = active_epoch
        .checked_add(1)
        .ok_or(RebuildError::Invalid("epoch overflow"))?;

    match (existing_build, existing_target) {
        (None, None) => {
            transaction.execute(
                "UPDATE source_usage_epochs
                 SET build_epoch=?1, build_parser_version=?2
                 WHERE source='codex' AND build_epoch IS NULL",
                params![build_epoch, target_parser_version],
            )?;
            freeze_initial_members(
                transaction,
                active_epoch,
                build_epoch,
                target_parser_version,
                present,
                now_ms,
            )?;
        }
        (Some(epoch), Some(parser)) if epoch == build_epoch && parser == target_parser_version => {
            add_new_present_members(
                transaction,
                active_epoch,
                build_epoch,
                target_parser_version,
                present,
                now_ms,
            )?;
        }
        (Some(epoch), Some(_)) if epoch == build_epoch => {
            replace_target_preserving_members(
                transaction,
                active_epoch,
                build_epoch,
                target_parser_version,
                present,
                now_ms,
            )?;
        }
        _ => return Err(RebuildError::Invalid("invalid app build pair")),
    }
    load_snapshot(transaction)
}

pub(crate) fn apply_codex_rebuild_record_progress(
    transaction: &Connection,
    progress: &SourceProgress,
) -> Result<ProgressOutcome, RebuildError> {
    let (build_epoch, parser_version) = current_build(transaction)?;
    let member = load_member_for_update(transaction, build_epoch, progress.source_file_id)?;
    if matches!(
        member.completion_status,
        CompletionStatus::Blocked | CompletionStatus::Quarantined
    ) {
        return Err(RebuildError::Cas(
            "blocked/quarantined source cannot record progress",
        ));
    }
    if member.completion_status == CompletionStatus::Carried {
        return Err(RebuildError::Cas(
            "carried source is owned by the carry protocol",
        ));
    }
    if member.expected_generation != progress.expected_generation
        || member.observed_raw_size != progress.observed_raw_size
    {
        return Err(RebuildError::Cas("manifest generation or raw size changed"));
    }
    verify_current_identity(transaction, &member)?;

    let checkpoint = transaction
        .query_row(
            "SELECT parser_version, committed_offset, guard_hash, processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [progress.source_file_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((checkpoint_parser, committed_offset, checkpoint_guard, checkpoint_status)) =
        checkpoint
    else {
        return Err(RebuildError::Cas("usage checkpoint missing"));
    };
    if checkpoint_parser != parser_version {
        return Err(RebuildError::Cas("checkpoint parser changed"));
    }
    if committed_offset == progress.last_complete_offset
        && state_matches_progress(transaction, build_epoch, progress)?
    {
        return Ok(ProgressOutcome::AlreadyApplied);
    }
    if checkpoint_guard != progress.expected_guard_hash {
        return Err(RebuildError::Cas("checkpoint guard changed"));
    }
    if committed_offset != progress.start_offset
        || (progress.start_offset == 0
            && checkpoint_status != "rebuild_required"
            && checkpoint_status != "ready")
        || (progress.start_offset > 0 && checkpoint_status != "ready")
    {
        return Err(RebuildError::Cas(
            "checkpoint is not the planned start boundary",
        ));
    }

    let (tail_status, tail_start, exhausted) = tail_columns(progress)?;
    let canonical_algorithm = crate::usage::canonical_algorithm_for(parser_version).ok_or(
        RebuildError::Invalid("unknown usage parser canonical algorithm"),
    )?;
    transaction.execute(
        "INSERT INTO usage_source_states (
            ledger_epoch, source_file_id, file_generation, device_id, inode,
            usage_parser_version, canonical_algorithm_version,
            resolved_through_offset, observed_raw_size, raw_tail_status,
            raw_tail_start_offset, owning_thread_id, root_session_id,
            continuation_state, chain_state, chain_block_reason, updated_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
                   'owning_live','continuous',NULL,?14)
         ON CONFLICT(ledger_epoch,source_file_id) DO UPDATE SET
            file_generation=excluded.file_generation,
            device_id=excluded.device_id,
            inode=excluded.inode,
            usage_parser_version=excluded.usage_parser_version,
            canonical_algorithm_version=excluded.canonical_algorithm_version,
            resolved_through_offset=excluded.resolved_through_offset,
            observed_raw_size=excluded.observed_raw_size,
            raw_tail_status=excluded.raw_tail_status,
            raw_tail_start_offset=excluded.raw_tail_start_offset,
            owning_thread_id=excluded.owning_thread_id,
            root_session_id=excluded.root_session_id,
            continuation_state='owning_live', chain_state='continuous',
            chain_block_reason=NULL, updated_at_ms=excluded.updated_at_ms",
        params![
            build_epoch,
            progress.source_file_id,
            member.expected_generation,
            member.expected_device_id,
            member.expected_inode,
            parser_version,
            canonical_algorithm,
            progress.last_complete_offset,
            progress.observed_raw_size,
            tail_status,
            tail_start,
            member.expected_owning_thread_id,
            member.expected_root_session_id,
            progress.updated_at_ms,
        ],
    )?;
    transaction.execute(
        "UPDATE source_checkpoints SET parser_version=?1, committed_offset=?2,
                guard_hash=?3, processing_status='ready',
                last_successful_scan_at_ms=?4, last_error_code=NULL
         WHERE source_file_id=?5 AND consumer_kind='usage'",
        params![
            parser_version,
            progress.last_complete_offset,
            progress.guard_hash,
            progress.updated_at_ms,
            progress.source_file_id,
        ],
    )?;
    let completion = if exhausted { "rebuilt" } else { "pending" };
    let changed = transaction.execute(
        "UPDATE usage_build_sources SET
            required_through_offset=MAX(required_through_offset,?1),
            raw_tail_status=?2, raw_tail_start_offset=?3,
            completion_status=?4, completion_error_code=NULL,
            completed_generation=CASE WHEN ?4='rebuilt' THEN required_generation ELSE NULL END,
            completed_through_offset=CASE WHEN ?4='rebuilt' THEN ?1 ELSE NULL END,
            updated_at_ms=?5
         WHERE build_epoch=?6 AND source_file_id=?7
           AND expected_file_generation=?8
           AND completion_status IN ('pending','rebuilt')",
        params![
            progress.last_complete_offset,
            tail_status,
            tail_start,
            completion,
            progress.updated_at_ms,
            build_epoch,
            progress.source_file_id,
            progress.expected_generation,
        ],
    )?;
    if changed != 1 {
        return Err(RebuildError::Cas("manifest completion CAS failed"));
    }
    verify_completion_row_for_storage(transaction, build_epoch, progress.source_file_id)?;
    Ok(if exhausted {
        ProgressOutcome::Rebuilt
    } else {
        ProgressOutcome::Advanced
    })
}

pub(crate) fn apply_codex_rebuild_quarantine_session(
    transaction: &Connection,
    root_session_id: &str,
    error_code: &str,
    now_ms: i64,
) -> Result<usize, RebuildError> {
    let (build_epoch, target_parser) = current_build(transaction)?;
    let mut statement = transaction.prepare(
        "SELECT source_file_id,expected_file_generation,expected_device_id,expected_inode,observed_raw_size
         FROM usage_build_sources
         WHERE build_epoch=?1 AND expected_root_session_id=?2
         ORDER BY source_file_id",
    )?;
    let members = statement
        .query_map(params![build_epoch, root_session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, u64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if members.is_empty() {
        return Err(RebuildError::Cas("session has no build members"));
    }

    let last_activity_at_ms: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(COALESCE(updated_at_ms,created_at_ms,0)),0)
         FROM threads WHERE thread_id=?1 OR root_session_id=?1",
        [root_session_id],
        |row| row.get(0),
    )?;

    transaction.execute(
        "INSERT INTO usage_session_quarantine(
            ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,
            first_seen_at_ms,updated_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?5)
         ON CONFLICT(ledger_epoch,root_session_id) DO UPDATE SET
            primary_error_code=excluded.primary_error_code,
            last_activity_at_ms=MAX(usage_session_quarantine.last_activity_at_ms,excluded.last_activity_at_ms),
            updated_at_ms=excluded.updated_at_ms",
        params![build_epoch, root_session_id, error_code, last_activity_at_ms, now_ms],
    )?;
    transaction.execute(
        "DELETE FROM usage_session_quarantine_sources
         WHERE ledger_epoch=?1 AND root_session_id=?2",
        params![build_epoch, root_session_id],
    )?;

    for (source_file_id, generation, device_id, inode, observed_size) in &members {
        cleanup_build_source(transaction, build_epoch, *source_file_id)?;
        reset_checkpoint(transaction, *source_file_id, target_parser)?;
        let changed = transaction.execute(
            "UPDATE usage_build_sources SET
                completion_status='quarantined',completion_error_code=?1,
                completed_generation=NULL,completed_through_offset=NULL,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?2
             WHERE build_epoch=?3 AND source_file_id=?4
               AND expected_root_session_id=?5",
            params![
                error_code,
                now_ms,
                build_epoch,
                source_file_id,
                root_session_id
            ],
        )?;
        if changed != 1 {
            return Err(RebuildError::Cas("session quarantine manifest CAS failed"));
        }
        transaction.execute(
            "INSERT INTO usage_session_quarantine_sources(
                ledger_epoch,root_session_id,source_file_id,file_generation,
                device_id,inode,observed_size,updated_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                build_epoch,
                root_session_id,
                source_file_id,
                generation,
                device_id,
                inode,
                observed_size,
                now_ms,
            ],
        )?;
    }

    let leaked: i64 = transaction.query_row(
        "SELECT
            (SELECT count(*) FROM usage_events
             WHERE source='codex' AND source_epoch=?1 AND root_session_id=?2)
          + (SELECT count(*) FROM turns WHERE ledger_epoch=?1 AND thread_id IN (
                SELECT thread_id FROM threads WHERE root_session_id=?2 OR thread_id=?2))
          + (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=?1 AND root_session_id=?2)",
        params![build_epoch, root_session_id],
        |row| row.get(0),
    )?;
    if leaked != 0 {
        return Err(RebuildError::Cas(
            "quarantined session still has build usage rows",
        ));
    }
    Ok(members.len())
}

pub(crate) fn apply_codex_replace_build_sources(
    transaction: &Connection,
    parser_version: i64,
    present: &BTreeSet<i64>,
    invalidated: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let (active, build): (i64, Option<i64>) = transaction.query_row(
        "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some(build) = build else {
        return Err(RebuildError::Invalid("no usage build to replace"));
    };
    replace_build_preserving_all_members_tx(
        transaction,
        active,
        build,
        parser_version,
        present,
        invalidated,
        now_ms,
    )
}

pub(crate) fn apply_codex_rebuild_block_source(
    transaction: &Connection,
    source_file_id: i64,
    error_code: &str,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let (build_epoch, _) = current_build(transaction)?;
    let changed = transaction.execute(
        "UPDATE usage_build_sources SET completion_status='blocked',
                completion_error_code=?1, completed_generation=NULL,
                completed_through_offset=NULL, updated_at_ms=?2
         WHERE build_epoch=?3 AND source_file_id=?4
           AND completion_status IN ('pending','blocked') AND carry_phase='none'",
        params![error_code, now_ms, build_epoch, source_file_id],
    )?;
    if changed != 1 {
        return Err(RebuildError::Cas("source cannot transition to blocked"));
    }
    Ok(())
}

pub(crate) fn apply_codex_rebuild_retry_blocked(
    transaction: &Connection,
    source_file_id: i64,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let (build_epoch, _) = current_build(transaction)?;
    let changed = transaction.execute(
        "UPDATE usage_build_sources SET completion_status='pending',
                completion_error_code=NULL, updated_at_ms=?1
         WHERE build_epoch=?2 AND source_file_id=?3
           AND completion_status='blocked' AND carry_phase='none'
           AND EXISTS (
               SELECT 1 FROM source_files sf
               WHERE sf.source_file_id=usage_build_sources.source_file_id
                 AND sf.file_status='present'
                 AND sf.file_generation=usage_build_sources.expected_file_generation
                 AND sf.device_id=usage_build_sources.expected_device_id
                 AND sf.inode=usage_build_sources.expected_inode
           )",
        params![now_ms, build_epoch, source_file_id],
    )?;
    if changed != 1 {
        return Err(RebuildError::Cas("blocked condition is not resolved"));
    }
    Ok(())
}

pub(crate) fn apply_codex_rebuild_activate(
    transaction: &Connection,
    present: &BTreeSet<i64>,
) -> Result<ActivationOutcome, RebuildError> {
    let (build_epoch, target_parser) = current_build(transaction)?;
    verify_complete_present_set(transaction, build_epoch, present)?;

    let unfinished: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_build_sources
         WHERE build_epoch=?1 AND completion_status NOT IN ('rebuilt','carried','quarantined')",
        [build_epoch],
        |row| row.get(0),
    )?;
    if unfinished != 0 {
        return Err(RebuildError::Cas("manifest contains unfinished sources"));
    }
    let member_ids = query_ids(
        transaction,
        "SELECT source_file_id FROM usage_build_sources WHERE build_epoch=?1
         ORDER BY source_file_id",
        build_epoch,
    )?;
    for source_file_id in member_ids {
        let status: String = transaction.query_row(
            "SELECT completion_status FROM usage_build_sources
             WHERE build_epoch=?1 AND source_file_id=?2",
            params![build_epoch, source_file_id],
            |row| row.get(0),
        )?;
        if status == "quarantined" {
            verify_quarantined_source(transaction, build_epoch, source_file_id)?;
        } else {
            verify_completion_row_for_storage(transaction, build_epoch, source_file_id)?;
        }
    }
    let (active_epoch, active_parser): (i64, i64) = transaction.query_row(
        "SELECT active_epoch,active_parser_version
         FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let visible_changed = !crate::usage::usage_epochs_visible_equal(
        transaction,
        "codex",
        active_epoch,
        active_parser,
        build_epoch,
        target_parser,
    )?;
    let changed = transaction.execute(
        "UPDATE source_usage_epochs
         SET active_epoch=?1, active_parser_version=?2,
             build_epoch=NULL, build_parser_version=NULL
         WHERE source='codex' AND build_epoch=?1 AND build_parser_version=?2",
        params![build_epoch, target_parser],
    )?;
    if changed != 1 {
        return Err(RebuildError::Cas("build changed before activation"));
    }
    if visible_changed {
        transaction.execute(
            "UPDATE app_meta SET data_revision=data_revision+1 WHERE id=1",
            [],
        )?;
    }
    transaction.execute(
        "DELETE FROM usage_build_sources WHERE build_epoch=?1",
        [build_epoch],
    )?;
    let data_revision =
        transaction.query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })?;
    Ok(ActivationOutcome {
        active_epoch: build_epoch,
        data_revision,
    })
}

#[derive(Debug)]
pub enum RebuildError {
    Sql(rusqlite::Error),
    Invalid(&'static str),
    Cas(&'static str),
}

impl fmt::Display for RebuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => error.fmt(formatter),
            Self::Invalid(message) | Self::Cas(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RebuildError {}

impl From<rusqlite::Error> for RebuildError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

#[derive(Clone, Debug)]
struct FrozenMember {
    source_file_id: i64,
    expected_generation: i64,
    expected_device_id: i64,
    expected_inode: i64,
    expected_owning_thread_id: Option<String>,
    expected_root_session_id: Option<String>,
    observed_raw_size: u64,
    active_committed_offset: u64,
    active_guard_hash: Option<Vec<u8>>,
    active_state_fingerprint: Option<Vec<u8>>,
    required_through_offset: u64,
    raw_tail_status: String,
    raw_tail_start_offset: Option<u64>,
    membership_reason: String,
    completion_status: CompletionStatus,
}

fn freeze_initial_members(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    parser: i64,
    present: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let active = active_contributors(transaction, active_epoch)?;
    let members = active.union(present).copied().collect::<Vec<_>>();
    for source_file_id in members {
        let member = freeze_member(
            transaction,
            active_epoch,
            source_file_id,
            active.contains(&source_file_id),
            present.contains(&source_file_id),
        )?;
        insert_manifest(transaction, build_epoch, parser, &member, now_ms)?;
        reset_checkpoint(transaction, source_file_id, parser)?;
    }
    Ok(())
}

fn add_new_present_members(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    parser: i64,
    present: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    for &source_file_id in present {
        let exists = transaction
            .query_row(
                "SELECT 1 FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_file_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            continue;
        }
        let mut member = freeze_member(transaction, active_epoch, source_file_id, false, true)?;
        member.membership_reason = "discovered_during_build".to_owned();
        insert_manifest(transaction, build_epoch, parser, &member, now_ms)?;
        reset_checkpoint(transaction, source_file_id, parser)?;
    }
    Ok(())
}

fn replace_target_preserving_members(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    parser: i64,
    present: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    // A parser/canonical target replacement invalidates every build-only
    // contribution, but it must keep the complete old membership set.  The
    // same primitive is also used by source/root replacements with an
    // explicit affected-source set via `replace_build_preserving_all_members`.
    let old_target: i64 = transaction.query_row(
        "SELECT build_parser_version FROM source_usage_epochs
         WHERE source='codex' AND build_epoch=?1",
        [build_epoch],
        |row| row.get(0),
    )?;
    let old = query_ids(
        transaction,
        "SELECT source_file_id FROM usage_build_sources WHERE build_epoch=?1 ORDER BY source_file_id",
        build_epoch,
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let active = active_contributors(transaction, active_epoch)?;
    let all = old
        .union(&active)
        .copied()
        .collect::<BTreeSet<_>>()
        .union(present)
        .copied()
        .collect::<BTreeSet<_>>();
    let invalidated = if old_target != parser {
        all.clone()
    } else {
        BTreeSet::new()
    };
    replace_build_preserving_all_members_tx(
        transaction,
        active_epoch,
        build_epoch,
        parser,
        present,
        &invalidated,
        now_ms,
    )
}

/// Replace a build target/source proof without ever dropping the old manifest
/// membership set. Unaffected rows are deliberately left byte-for-byte
/// intact, including pending-ready progress, completion proofs and carry
/// cursors. Only invalidated/new members are initialized from zero.
pub(crate) fn replace_build_preserving_all_members_tx(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    parser: i64,
    present: &BTreeSet<i64>,
    invalidated: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    if parser < 0 || now_ms < 0 {
        return Err(RebuildError::Invalid("negative parser version or time"));
    }
    let current_build: Option<i64> = transaction.query_row(
        "SELECT build_epoch FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| row.get(0),
    )?;
    if current_build != Some(build_epoch) {
        return Err(RebuildError::Cas("build changed before replacement"));
    }

    let old_ids = query_ids(
        transaction,
        "SELECT source_file_id FROM usage_build_sources WHERE build_epoch=?1 ORDER BY source_file_id",
        build_epoch,
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let active_ids = active_contributors(transaction, active_epoch)?;
    let all_ids = old_ids
        .union(&active_ids)
        .copied()
        .collect::<BTreeSet<_>>()
        .union(present)
        .copied()
        .collect::<BTreeSet<_>>();

    let old_target: i64 = transaction.query_row(
        "SELECT build_parser_version FROM source_usage_epochs
         WHERE source='codex' AND build_epoch=?1",
        [build_epoch],
        |row| row.get(0),
    )?;
    let parser_changed = old_target != parser;
    transaction.execute(
        "UPDATE source_usage_epochs SET build_parser_version=?1
         WHERE source='codex' AND build_epoch=?2",
        params![parser, build_epoch],
    )?;

    // Capture membership reasons before selectively replacing rows.
    let mut old_reasons = BTreeMap::new();
    {
        let mut statement = transaction.prepare(
            "SELECT source_file_id,membership_reason FROM usage_build_sources WHERE build_epoch=?1",
        )?;
        let rows = statement.query_map([build_epoch], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (source_id, reason) = row?;
            old_reasons.insert(source_id, reason);
        }
    }

    for source_file_id in all_ids {
        let existed = old_ids.contains(&source_file_id);
        let must_reset = parser_changed || invalidated.contains(&source_file_id) || !existed;
        if !must_reset {
            // The defining property of replacement preservation: do not touch
            // a safe member's build rows, state, checkpoint or manifest row.
            continue;
        }

        // A parser-target replacement resets build-only rows, but it must not
        // erase the frozen active proof that the old manifest already captured.
        // The shared usage checkpoint may currently point at build progress, so
        // re-freezing from it after cleanup would incorrectly turn an active
        // contributor's required boundary into zero. Preserve the old active
        // proof only when the frozen identity/binding is still the same; an
        // identity/binding replacement intentionally cannot carry that proof.
        let old_proof: Option<OldBuildProof> = if existed {
            transaction
                .query_row(
                    "SELECT expected_file_generation,expected_device_id,expected_inode,
                            expected_owning_thread_id,expected_root_session_id,
                            active_committed_offset,active_guard_hash,active_state_fingerprint,
                            required_through_offset,observed_raw_size,raw_tail_status,
                            raw_tail_start_offset
                     FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
                    params![build_epoch, source_file_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                            row.get(10)?,
                            row.get(11)?,
                        ))
                    },
                )
                .optional()?
        } else {
            None
        };

        cleanup_build_source(transaction, build_epoch, source_file_id)?;
        transaction.execute(
            "DELETE FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
            params![build_epoch, source_file_id],
        )?;

        let mut member = freeze_member(
            transaction,
            active_epoch,
            source_file_id,
            active_ids.contains(&source_file_id),
            present.contains(&source_file_id),
        )?;
        if let Some(reason) = old_reasons.get(&source_file_id) {
            member.membership_reason = reason.clone();
        } else if !active_ids.contains(&source_file_id) && present.contains(&source_file_id) {
            member.membership_reason = "discovered_during_build".to_owned();
        }
        if let Some(old) = old_proof {
            let same_generation = old.0 == member.expected_generation;
            let same_physical_generation = same_generation
                && old.1 == member.expected_device_id
                && old.2 == member.expected_inode;
            let same_frozen_identity = same_physical_generation
                && old.3 == member.expected_owning_thread_id
                && old.4 == member.expected_root_session_id;

            // required_through_offset is a byte-boundary fact of a generation,
            // not of the current root/binding. A metadata replacement in the
            // same generation must still rebuild through every complete byte
            // boundary already observed by the old build.
            if same_generation {
                member.required_through_offset = member.required_through_offset.max(old.8);
            }
            if same_frozen_identity {
                member.active_committed_offset = old.5;
                member.active_guard_hash = old.6.clone();
                member.active_state_fingerprint = old.7.clone();
            } else if same_physical_generation && active_epoch > 0 {
                // A confirmed root reconciliation changes only logical
                // attribution. The physical active checkpoint boundary and
                // guard remain valid, but the source-state fingerprint must be
                // recomputed after the active state has been rewritten to the
                // new root. The shared checkpoint may currently belong to the
                // build epoch, so it cannot be used to reconstruct this proof.
                let active_parser: i64 = transaction.query_row(
                    "SELECT active_parser_version FROM source_usage_epochs
             WHERE source='codex'",
                    [],
                    |row| row.get(0),
                )?;
                let active_matches: i64 = transaction.query_row(
                    "SELECT count(*) FROM usage_source_states
                     WHERE ledger_epoch=?1 AND source_file_id=?2
                       AND file_generation=?3 AND device_id=?4 AND inode=?5
                       AND usage_parser_version=?6 AND canonical_algorithm_version=?7
                       AND resolved_through_offset=?8
                       AND owning_thread_id=?9 AND root_session_id=?10",
                    params![
                        active_epoch,
                        source_file_id,
                        member.expected_generation,
                        member.expected_device_id,
                        member.expected_inode,
                        active_parser,
                        crate::usage::canonical_algorithm_for(active_parser).unwrap_or(-1),
                        old.5,
                        member
                            .expected_owning_thread_id
                            .as_deref()
                            .unwrap_or_default(),
                        member
                            .expected_root_session_id
                            .as_deref()
                            .unwrap_or_default(),
                    ],
                    |row| row.get(0),
                )?;
                if active_matches == 1 {
                    member.active_committed_offset = old.5;
                    member.active_guard_hash = old.6.clone();
                    member.active_state_fingerprint =
                        active_state_fingerprint(transaction, active_epoch, source_file_id)?;
                }
            }
            // Raw-tail framing proof is likewise relationship/parser
            // independent, but it is valid only for the same physical
            // generation and raw size.
            if same_physical_generation && member.observed_raw_size == old.9 {
                member.raw_tail_status = old.10;
                member.raw_tail_start_offset = old.11;
            }
        }
        insert_manifest(transaction, build_epoch, parser, &member, now_ms)?;
        reset_checkpoint(transaction, source_file_id, parser)?;
    }
    Ok(())
}

pub(crate) fn cleanup_build_source(
    transaction: &Connection,
    build_epoch: i64,
    source_file_id: i64,
) -> Result<(), RebuildError> {
    transaction.execute(
        "DELETE FROM usage_event_occurrences WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
    )?;
    // Canonical rows are source-aware. Delete only Codex build rows no longer
    // referenced by any Codex occurrence in the same source epoch.
    transaction.execute(
        "DELETE FROM usage_events
         WHERE source='codex' AND source_epoch=?1
           AND NOT EXISTS (
               SELECT 1 FROM usage_event_occurrences o
               WHERE o.source='codex' AND o.ledger_epoch=usage_events.source_epoch
                 AND o.event_id=usage_events.event_id
           )",
        [build_epoch],
    )?;
    Ok(())
}

/// Apply the Spec04 build-manifest side of one Spec01 source-observation
/// transaction. The caller has already written `source_files` (including
/// missing transitions) but has not committed the transaction yet.
pub(crate) fn apply_source_observations_to_build_tx(
    transaction: &Connection,
    observed_source_ids: &[i64],
    results: &mut [crate::domain::SourceObservationResult],
    usage_carry_proofs: &std::collections::HashMap<
        (i64, i64),
        &crate::storage::source::UsageCarryObservationProof,
    >,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let (active_epoch, build_epoch, target_parser): (i64, Option<i64>, Option<i64>) = transaction
        .query_row(
        "SELECT active_epoch,build_epoch,build_parser_version
             FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let Some(build_epoch) = build_epoch else {
        return Ok(());
    };
    let target_parser = target_parser.ok_or(RebuildError::Invalid("invalid build parser pair"))?;
    let present = observed_source_ids.iter().copied().collect::<BTreeSet<_>>();

    for result in results.iter_mut() {
        let source_id = result.source_file_id;
        let source: (i64, i64, i64, u64, String, Option<String>, Option<String>) = transaction
            .query_row(
                "SELECT sf.file_generation,sf.device_id,sf.inode,sf.observed_size,sf.file_status,
                        sf.thread_id,t.root_session_id
                 FROM source_files sf LEFT JOIN threads t ON t.thread_id=sf.thread_id
                 WHERE sf.source_file_id=?1",
                [source_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
        let member: Option<BuildMemberRow> = transaction
            .query_row(
                "SELECT expected_file_generation,expected_device_id,expected_inode,
                        expected_owning_thread_id,expected_root_session_id,
                        required_through_offset,observed_raw_size,raw_tail_status,
                        raw_tail_start_offset,completion_status,carry_phase,
                        active_committed_offset,active_guard_hash,target_parser_version
                 FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                    ))
                },
            )
            .optional()?;

        let Some(member) = member else {
            let mut frozen = freeze_member(transaction, active_epoch, source_id, false, true)?;
            frozen.membership_reason = "discovered_during_build".to_owned();
            insert_manifest(transaction, build_epoch, target_parser, &frozen, now_ms)?;
            reset_checkpoint(transaction, source_id, target_parser)?;
            result.build_disposition = crate::domain::BuildDisposition::MemberAdded;
            continue;
        };

        let carry_guard_matches = if member.10 == "none" {
            true
        } else if member.11 == 0 {
            member.12.is_none()
        } else {
            member.12.is_some()
                && usage_carry_proofs
                    .get(&(source.1, source.2))
                    .is_some_and(|proof| {
                        proof.active_committed_offset == i64::try_from(member.11).unwrap_or(-1)
                            && proof.guard_matches
                    })
        };
        let incompatible = result.replaced
            || member.0 != source.0
            || member.1 != source.1
            || member.2 != source.2
            || member.3 != source.5
            || member.4 != source.6
            || member.13 != target_parser
            || !carry_guard_matches;
        if incompatible {
            let invalidated = [source_id].into_iter().collect::<BTreeSet<_>>();
            replace_build_preserving_all_members_tx(
                transaction,
                active_epoch,
                build_epoch,
                target_parser,
                &present,
                &invalidated,
                now_ms,
            )?;
            result.build_disposition = crate::domain::BuildDisposition::Replaced;
            continue;
        }

        if member.9 == "carried" && source.4 == "present" {
            // Carried is a completion proof only while the source remains
            // missing. A same-identity reappearance keeps the copied prefix
            // and durable tail proof, but must become pending so CompleteOnly
            // or BuildFrom can establish a present-source Rebuilt proof.
            transaction.execute(
                "UPDATE usage_build_sources SET completion_status='pending',
                        completion_error_code=NULL,completed_generation=NULL,
                        completed_through_offset=NULL,updated_at_ms=?3
                 WHERE build_epoch=?1 AND source_file_id=?2 AND completion_status='carried'",
                params![build_epoch, source_id, now_ms],
            )?;
            result.build_disposition = crate::domain::BuildDisposition::CompletionInvalidated;
        }

        if member.10 != "none" {
            // A present return during carry never discards the durable cursor.
            // Identity/binding were checked above; finalize will restore the
            // active prefix and then the file reader continues at that offset.
            let tail_changed = member.6 != source.3;
            transaction.execute(
                "UPDATE usage_build_sources SET observed_raw_size=?3,
                        raw_tail_status=CASE WHEN ?4 THEN 'unverified' ELSE raw_tail_status END,
                        raw_tail_start_offset=CASE WHEN ?4 THEN NULL ELSE raw_tail_start_offset END,
                        updated_at_ms=?5
                 WHERE build_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_id, source.3, tail_changed, now_ms],
            )?;
            result.build_disposition = crate::domain::BuildDisposition::CarryResumedPresent;
            continue;
        }

        if member.9 == "carried" {
            // A carried member is complete only while the source remains
            // missing.  Reappearance under the same frozen identity restores
            // the ready active-prefix checkpoint/state produced by finalize and
            // makes the member pending so the reader can verify/consume any
            // bytes after that prefix.  A changed raw size invalidates only the
            // tail proof; an identical raw view may retain its durable proof.
            let tail_changed = member.6 != source.3;
            transaction.execute(
                "UPDATE usage_build_sources SET observed_raw_size=?3,
                        raw_tail_status=CASE WHEN ?4 THEN 'unverified' ELSE raw_tail_status END,
                        raw_tail_start_offset=CASE WHEN ?4 THEN NULL ELSE raw_tail_start_offset END,
                        completion_status='pending',completion_error_code=NULL,
                        completed_generation=NULL,completed_through_offset=NULL,
                        updated_at_ms=?5
                 WHERE build_epoch=?1 AND source_file_id=?2 AND completion_status='carried'",
                params![build_epoch, source_id, source.3, tail_changed, now_ms],
            )?;
            result.build_disposition = crate::domain::BuildDisposition::CompletionInvalidated;
            continue;
        }

        if member.6 != source.3 {
            // Same generation append/growth keeps all proven build facts and
            // checkpoint state, but invalidates only the completion/raw-tail
            // proof. required_through_offset is advanced only by a real reader
            // commit, never by observation alone.
            transaction.execute(
                "UPDATE usage_build_sources SET observed_raw_size=?3,
                        raw_tail_status='unverified',raw_tail_start_offset=NULL,
                        completion_status='pending',completion_error_code=NULL,
                        completed_generation=NULL,completed_through_offset=NULL,
                        updated_at_ms=?4
                 WHERE build_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_id, source.3, now_ms],
            )?;
            result.build_disposition = crate::domain::BuildDisposition::CompletionInvalidated;
        }
    }

    // Missing transitions are part of this same transaction. Never remove a
    // member; an unfinished missing member is blocked until BeginCarry proves
    // it can be copied from active.
    transaction.execute(
        "UPDATE usage_build_sources AS b
         SET completion_status=CASE
                WHEN completion_status IN ('pending','blocked') THEN 'blocked'
                ELSE completion_status END,
             completion_error_code=CASE
                WHEN completion_status IN ('pending','blocked') THEN 'CARRY_REQUIRED'
                ELSE completion_error_code END,
             updated_at_ms=?2
         WHERE b.build_epoch=?1 AND b.carry_phase='none'
           AND EXISTS (SELECT 1 FROM source_files sf
                       WHERE sf.source_file_id=b.source_file_id AND sf.file_status='missing')",
        params![build_epoch, now_ms],
    )?;
    Ok(())
}

fn active_contributors(
    transaction: &Connection,
    active_epoch: i64,
) -> Result<BTreeSet<i64>, RebuildError> {
    if active_epoch == 0 {
        return Ok(BTreeSet::new());
    }
    let mut statement = transaction.prepare(
        "SELECT source_file_id FROM usage_event_occurrences WHERE ledger_epoch=?1
         UNION SELECT source_file_id FROM turns WHERE ledger_epoch=?1
         UNION SELECT source_file_id FROM ingest_anomalies
               WHERE ledger_epoch=?1 AND source_file_id IS NOT NULL
         UNION SELECT source_file_id FROM usage_source_states WHERE ledger_epoch=?1",
    )?;
    let rows = statement.query_map([active_epoch], |row| row.get(0))?;
    rows.collect::<Result<BTreeSet<_>, _>>().map_err(Into::into)
}

fn freeze_member(
    transaction: &Connection,
    active_epoch: i64,
    source_file_id: i64,
    active: bool,
    present_in_round: bool,
) -> Result<FrozenMember, RebuildError> {
    let source = transaction.query_row(
        "SELECT sf.file_generation,sf.device_id,sf.inode,sf.observed_size,
                sf.file_status,sf.thread_id,t.root_session_id
         FROM source_files sf LEFT JOIN threads t ON t.thread_id=sf.thread_id
         WHERE sf.source_file_id=?1",
        [source_file_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        },
    )?;
    if present_in_round && source.4 != "present" {
        return Err(RebuildError::Invalid(
            "complete present set contains a missing source",
        ));
    }
    let checkpoint = if active_epoch > 0 {
        transaction
            .query_row(
                "SELECT committed_offset,guard_hash,parser_version,processing_status FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
                [source_file_id],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?,
                          row.get::<_, i64>(2)?, row.get::<_, String>(3)?)),
            )
            .optional()?
    } else {
        None
    };
    let state = if active_epoch > 0 {
        transaction
            .query_row(
                "SELECT resolved_through_offset,observed_raw_size,raw_tail_status,
                    raw_tail_start_offset,file_generation,device_id,inode,
                    owning_thread_id,root_session_id,usage_parser_version,canonical_algorithm_version
             FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
                params![active_epoch, source_file_id],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<u64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                    ))
                },
            )
            .optional()?
    } else {
        None
    };
    let active_parser: i64 = if active_epoch > 0 {
        transaction.query_row(
            "SELECT active_parser_version FROM source_usage_epochs
                     WHERE source='codex'",
            [],
            |row| row.get(0),
        )?
    } else {
        0
    };
    // Active epochs may have been produced by an older parser that this
    // binary intentionally no longer accepts for new writes. We still freeze
    // its durable offset/identity proof so membership and required boundaries
    // cannot shrink. Carry eligibility separately requires target==active
    // parser and the current canonical mapping, so an old parser is never
    // silently reused.
    let state_matches = state.as_ref().is_some_and(|state| {
        state.4 == source.0
            && state.5 == source.1
            && state.6 == source.2
            && state.9 == active_parser
            && state.10 > 0
            && Some(&state.7) == source.5.as_ref()
            && Some(&state.8) == source.6.as_ref()
            && checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.0 == state.0
                    && checkpoint.2 == active_parser
                    && checkpoint.3 == "ready"
                    && (checkpoint.0 == 0) == checkpoint.1.is_none()
            })
    });
    let (active_offset, active_guard) = if state_matches {
        checkpoint
            .as_ref()
            .map(|value| (value.0, value.1.clone()))
            .unwrap_or((0, None))
    } else {
        (0, None)
    };
    // The manifest's required complete boundary is a durable framing fact of
    // the active source generation.  It is frozen even when the target parser
    // differs (that mismatch merely prevents carry).  Carry-specific active
    // offset/guard/state fingerprint above intentionally remain gated by the
    // stricter checkpoint/parser/binding proof.
    let durable_state = state.as_ref().filter(|state| {
        state.4 == source.0
            && state.5 == source.1
            && state.6 == source.2
            && state.1 == source.3
            && ((state.2 == "none" && state.0 == source.3 && state.3.is_none())
                || (state.2 == "half_line" && state.3 == Some(state.0) && state.0 < source.3))
    });
    let (required, tail_status, tail_start) = match durable_state {
        Some(state) => (state.0, state.2.clone(), state.3),
        None => (0, "unverified".to_owned(), None),
    };
    let state_fingerprint = if state_matches {
        active_state_fingerprint(transaction, active_epoch, source_file_id)?
    } else {
        None
    };
    let membership_reason = match (active, present_in_round) {
        (true, true) => "both",
        (true, false) => "active_contributor",
        (false, true) => "present_at_build_start",
        (false, false) => "active_contributor",
    }
    .to_owned();
    let blocked = source.4 != "present" || source.5.is_none() || source.6.is_none();
    Ok(FrozenMember {
        source_file_id,
        expected_generation: source.0,
        expected_device_id: source.1,
        expected_inode: source.2,
        expected_owning_thread_id: source.5,
        expected_root_session_id: source.6,
        observed_raw_size: source.3,
        active_committed_offset: active_offset,
        active_guard_hash: active_guard,
        active_state_fingerprint: state_fingerprint,
        required_through_offset: required,
        raw_tail_status: tail_status,
        raw_tail_start_offset: tail_start,
        membership_reason,
        completion_status: if blocked {
            CompletionStatus::Blocked
        } else {
            CompletionStatus::Pending
        },
    })
}

pub(crate) fn active_state_fingerprint(
    transaction: &Connection,
    active_epoch: i64,
    source_file_id: i64,
) -> Result<Option<Vec<u8>>, RebuildError> {
    if active_epoch <= 0 {
        return Ok(None);
    }
    let mut statement = transaction.prepare(
        "SELECT file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
                resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,
                owning_thread_id,root_session_id,continuation_state,
                previous_total_input_tokens,previous_total_cached_tokens,
                previous_total_cache_write_tokens,previous_total_output_tokens,
                previous_total_reasoning_tokens,previous_total_total_tokens,
                previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
                active_turn_key,active_model,active_model_offset,
                active_reasoning_effort,active_reasoning_effort_offset
         FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
    )?;
    let result = statement
        .query_row(params![active_epoch, source_file_id], |row| {
            use rusqlite::types::ValueRef;
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"usage-source-state-proof-v2");
            for index in 0..27 {
                match row.get_ref(index)? {
                    ValueRef::Null => {
                        hasher.update(&[0]);
                    }
                    ValueRef::Integer(value) => {
                        hasher.update(&[1]);
                        hasher.update(&value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        hasher.update(&[2]);
                        hasher.update(&value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        hasher.update(&[3]);
                        hasher.update(&(value.len() as u64).to_be_bytes());
                        hasher.update(value);
                    }
                    ValueRef::Blob(value) => {
                        hasher.update(&[4]);
                        hasher.update(&(value.len() as u64).to_be_bytes());
                        hasher.update(value);
                    }
                };
            }
            Ok(hasher.finalize().as_bytes().to_vec())
        })
        .optional()?;
    Ok(result)
}

fn insert_manifest(
    transaction: &Connection,
    build_epoch: i64,
    parser: i64,
    member: &FrozenMember,
    now_ms: i64,
) -> Result<(), RebuildError> {
    transaction.execute(
        "INSERT INTO usage_build_sources (
            build_epoch,source_file_id,target_parser_version,
            expected_file_generation,expected_device_id,expected_inode,
            expected_owning_thread_id,expected_root_session_id,
            active_committed_offset,active_guard_hash,active_state_fingerprint,
            required_generation,required_through_offset,observed_raw_size,
            raw_tail_status,raw_tail_start_offset,membership_reason,
            completion_status,completion_error_code,completed_generation,
            completed_through_offset,carry_phase,created_at_ms,updated_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?4,?12,?13,
                   ?14,?15,?16,?17,?18,NULL,NULL,'none',?19,?19)",
        params![
            build_epoch,
            member.source_file_id,
            parser,
            member.expected_generation,
            member.expected_device_id,
            member.expected_inode,
            member.expected_owning_thread_id,
            member.expected_root_session_id,
            member.active_committed_offset,
            member.active_guard_hash,
            member.active_state_fingerprint,
            member.required_through_offset,
            member.observed_raw_size,
            member.raw_tail_status,
            member.raw_tail_start_offset,
            member.membership_reason,
            if member.completion_status == CompletionStatus::Blocked {
                "blocked"
            } else {
                "pending"
            },
            if member.completion_status == CompletionStatus::Blocked {
                Some("CARRY_REQUIRED")
            } else {
                None
            },
            now_ms,
        ],
    )?;
    Ok(())
}

fn reset_checkpoint(
    transaction: &Connection,
    source_file_id: i64,
    parser: i64,
) -> Result<(), RebuildError> {
    transaction.execute(
        "INSERT INTO source_checkpoints (
            source_file_id,consumer_kind,parser_version,committed_offset,
            guard_hash,processing_status,last_successful_scan_at_ms,last_error_code
         ) VALUES (?1,'usage',?2,0,NULL,'rebuild_required',NULL,NULL)
         ON CONFLICT(source_file_id,consumer_kind) DO UPDATE SET
            parser_version=excluded.parser_version, committed_offset=0,
            guard_hash=NULL, processing_status='rebuild_required',
            last_successful_scan_at_ms=NULL,last_error_code=NULL",
        params![source_file_id, parser],
    )?;
    Ok(())
}

fn current_build(transaction: &Connection) -> Result<(i64, i64), RebuildError> {
    let pair = transaction.query_row(
        "SELECT build_epoch,build_parser_version FROM source_usage_epochs
         WHERE source='codex'",
        [],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
    )?;
    pair.0
        .zip(pair.1)
        .ok_or(RebuildError::Cas("no active build"))
}

fn load_snapshot(transaction: &Connection) -> Result<BuildSnapshot, RebuildError> {
    let (active_epoch, build_epoch, parser): (i64, i64, i64) = transaction.query_row(
        "SELECT active_epoch,build_epoch,build_parser_version
         FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT source_file_id,expected_file_generation,expected_device_id,expected_inode,
                required_through_offset,observed_raw_size,membership_reason,
                completion_status,completion_error_code,completed_through_offset
         FROM usage_build_sources WHERE build_epoch=?1 ORDER BY source_file_id",
    )?;
    let members = statement
        .query_map([build_epoch], |row| {
            let completion: String = row.get(7)?;
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, u64>(4)?,
                row.get::<_, u64>(5)?,
                row.get::<_, String>(6)?,
                completion,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<u64>>(9)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(ManifestEntry {
                source_file_id: row.0,
                expected_generation: row.1,
                expected_device_id: row.2,
                expected_inode: row.3,
                required_through_offset: row.4,
                observed_raw_size: row.5,
                membership_reason: row.6,
                completion_status: CompletionStatus::parse(&row.7)?,
                completion_error_code: row.8,
                completed_through_offset: row.9,
            })
        })
        .collect::<Result<Vec<_>, RebuildError>>()?;
    Ok(BuildSnapshot {
        active_epoch,
        build_epoch,
        target_parser_version: parser,
        members,
    })
}

fn load_member_for_update(
    transaction: &Connection,
    build_epoch: i64,
    source_file_id: i64,
) -> Result<FrozenMember, RebuildError> {
    let row = transaction
        .query_row(
            "SELECT expected_file_generation,expected_device_id,expected_inode,
                expected_owning_thread_id,expected_root_session_id,observed_raw_size,
                active_committed_offset,active_guard_hash,active_state_fingerprint,required_through_offset,
                raw_tail_status,raw_tail_start_offset,membership_reason,completion_status
         FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
            params![build_epoch, source_file_id],
            |row| {
                let status: String = row.get(13)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, u64>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                    row.get::<_, u64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<u64>>(11)?,
                    row.get::<_, String>(12)?,
                    status,
                ))
            },
        )
        .optional()?
        .ok_or(RebuildError::Cas("source is not a build member"))?;
    Ok(FrozenMember {
        source_file_id,
        expected_generation: row.0,
        expected_device_id: row.1,
        expected_inode: row.2,
        expected_owning_thread_id: row.3,
        expected_root_session_id: row.4,
        observed_raw_size: row.5,
        active_committed_offset: row.6,
        active_guard_hash: row.7,
        active_state_fingerprint: row.8,
        required_through_offset: row.9,
        raw_tail_status: row.10,
        raw_tail_start_offset: row.11,
        membership_reason: row.12,
        completion_status: CompletionStatus::parse(&row.13)?,
    })
}

fn verify_current_identity(
    transaction: &Connection,
    member: &FrozenMember,
) -> Result<(), RebuildError> {
    let matches: i64 = transaction.query_row(
        "SELECT count(*) FROM source_files sf
         LEFT JOIN threads t ON t.thread_id=sf.thread_id
         WHERE sf.source_file_id=?1 AND sf.file_status='present'
           AND sf.file_generation=?2 AND sf.device_id=?3 AND sf.inode=?4
           AND sf.thread_id IS ?5 AND t.root_session_id IS ?6",
        params![
            member.source_file_id,
            member.expected_generation,
            member.expected_device_id,
            member.expected_inode,
            member.expected_owning_thread_id,
            member.expected_root_session_id
        ],
        |row| row.get(0),
    )?;
    if matches != 1 {
        return Err(RebuildError::Cas("frozen source identity changed"));
    }
    Ok(())
}

fn validate_progress_shape(progress: &SourceProgress) -> Result<(), RebuildError> {
    if progress.source_file_id <= 0
        || progress.expected_generation <= 0
        || progress.start_offset > progress.last_complete_offset
        || progress.last_complete_offset > progress.observed_raw_size
        || progress.updated_at_ms < 0
        || (progress.start_offset == 0) != progress.expected_guard_hash.is_none()
    {
        return Err(RebuildError::Invalid("invalid source progress"));
    }
    Ok(())
}

fn tail_columns(
    progress: &SourceProgress,
) -> Result<(&'static str, Option<u64>, bool), RebuildError> {
    match progress.tail {
        TailProof::Unverified => Ok(("unverified", None, false)),
        TailProof::None if progress.last_complete_offset == progress.observed_raw_size => {
            Ok(("none", None, true))
        }
        TailProof::HalfLine { start_offset }
            if start_offset == progress.last_complete_offset
                && start_offset < progress.observed_raw_size =>
        {
            Ok(("half_line", Some(start_offset), true))
        }
        TailProof::None | TailProof::HalfLine { .. } => Err(RebuildError::Invalid(
            "tail proof does not match fixed view",
        )),
    }
}

fn state_matches_progress(
    transaction: &Connection,
    build_epoch: i64,
    progress: &SourceProgress,
) -> Result<bool, RebuildError> {
    let expected_tail = match progress.tail {
        TailProof::Unverified => ("unverified", None),
        TailProof::None => ("none", None),
        TailProof::HalfLine { start_offset } => ("half_line", Some(start_offset)),
    };
    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_source_states
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
           AND resolved_through_offset=?4 AND observed_raw_size=?5
           AND raw_tail_status=?6 AND raw_tail_start_offset IS ?7",
        params![
            build_epoch,
            progress.source_file_id,
            progress.expected_generation,
            progress.last_complete_offset,
            progress.observed_raw_size,
            expected_tail.0,
            expected_tail.1
        ],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn verify_quarantined_source(
    transaction: &Connection,
    build_epoch: i64,
    source_file_id: i64,
) -> Result<(), RebuildError> {
    let valid: i64 = transaction.query_row(
        "SELECT count(*)
         FROM usage_build_sources b
         JOIN usage_session_quarantine q
           ON q.ledger_epoch=b.build_epoch AND q.root_session_id=b.expected_root_session_id
         JOIN usage_session_quarantine_sources qs
           ON qs.ledger_epoch=b.build_epoch
          AND qs.root_session_id=b.expected_root_session_id
          AND qs.source_file_id=b.source_file_id
         WHERE b.build_epoch=?1 AND b.source_file_id=?2
           AND b.completion_status='quarantined'
           AND b.completion_error_code=q.primary_error_code
           AND qs.file_generation=b.expected_file_generation
           AND qs.device_id=b.expected_device_id AND qs.inode=b.expected_inode
           AND qs.observed_size=b.observed_raw_size
           AND NOT EXISTS (
               SELECT 1 FROM usage_event_occurrences o
               WHERE o.ledger_epoch=b.build_epoch AND o.source_file_id=b.source_file_id)
           AND NOT EXISTS (
               SELECT 1 FROM turns t
               WHERE t.ledger_epoch=b.build_epoch AND t.source_file_id=b.source_file_id)
           AND NOT EXISTS (
               SELECT 1 FROM usage_source_states st
               WHERE st.ledger_epoch=b.build_epoch AND st.source_file_id=b.source_file_id)",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    if valid != 1 {
        return Err(RebuildError::Cas("quarantined source proof is stale"));
    }
    Ok(())
}

fn query_ids_string(
    transaction: &Connection,
    sql: &str,
    value: &str,
) -> Result<Vec<i64>, RebuildError> {
    let mut statement = transaction.prepare(sql)?;
    let rows = statement.query_map([value], |row| row.get::<_, i64>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn verify_completion_row_for_storage(
    transaction: &Connection,
    build_epoch: i64,
    source_file_id: i64,
) -> Result<(), RebuildError> {
    let valid: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_build_sources m
         JOIN source_files sf ON sf.source_file_id=m.source_file_id
         JOIN source_checkpoints cp ON cp.source_file_id=m.source_file_id
              AND cp.consumer_kind='usage'
         JOIN usage_source_states st ON st.ledger_epoch=m.build_epoch
              AND st.source_file_id=m.source_file_id
         WHERE m.build_epoch=?1 AND m.source_file_id=?2
           AND m.completion_status IN ('pending','rebuilt','carried')
           AND sf.file_generation=m.expected_file_generation
           AND sf.device_id=m.expected_device_id AND sf.inode=m.expected_inode
           AND cp.parser_version=m.target_parser_version AND cp.processing_status='ready'
           AND cp.committed_offset=st.resolved_through_offset
           AND st.file_generation=m.expected_file_generation
           AND st.device_id=m.expected_device_id AND st.inode=m.expected_inode
           AND st.usage_parser_version=m.target_parser_version
           AND st.observed_raw_size=m.observed_raw_size
           AND st.owning_thread_id IS m.expected_owning_thread_id
           AND st.root_session_id IS m.expected_root_session_id
           AND st.continuation_state IN ('replayed_ancestor','owning_live')
           AND st.raw_tail_status=m.raw_tail_status
           AND st.raw_tail_start_offset IS m.raw_tail_start_offset
           AND (m.completion_status<>'carried' OR sf.file_status='missing')
           AND (m.completion_status='pending' OR (
                m.completed_generation=m.required_generation
                AND m.completed_through_offset=st.resolved_through_offset
                AND st.resolved_through_offset>=m.required_through_offset
                AND m.raw_tail_status IN ('none','half_line'))) ",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    if valid != 1 {
        return Err(RebuildError::Cas("source completion proof is stale"));
    }
    let (target_parser, canonical): (i64, i64) = transaction.query_row(
        "SELECT m.target_parser_version,st.canonical_algorithm_version
         FROM usage_build_sources m
         JOIN usage_source_states st ON st.ledger_epoch=m.build_epoch
              AND st.source_file_id=m.source_file_id
         WHERE m.build_epoch=?1 AND m.source_file_id=?2",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if crate::usage::canonical_algorithm_for(target_parser) != Some(canonical) {
        return Err(RebuildError::Cas(
            "source canonical algorithm proof is stale",
        ));
    }
    Ok(())
}

fn verify_complete_present_set(
    transaction: &Connection,
    build_epoch: i64,
    supplied: &BTreeSet<i64>,
) -> Result<(), RebuildError> {
    let actual = query_ids_no_param(
        transaction,
        "SELECT source_file_id FROM source_files WHERE file_status='present' ORDER BY source_file_id",
    )?.into_iter().collect::<BTreeSet<_>>();
    if &actual != supplied {
        return Err(RebuildError::Cas("discovery proof is incomplete"));
    }
    let manifest = query_ids(
        transaction,
        "SELECT source_file_id FROM usage_build_sources WHERE build_epoch=?1 ORDER BY source_file_id",
        build_epoch,
    )?.into_iter().collect::<BTreeSet<_>>();
    if !supplied.is_subset(&manifest) {
        return Err(RebuildError::Cas("present source missing from manifest"));
    }
    let active_epoch: i64 = transaction.query_row(
        "SELECT active_epoch FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| row.get(0),
    )?;
    let active = active_contributors(transaction, active_epoch)?;
    if !active.is_subset(&manifest) {
        return Err(RebuildError::Cas(
            "active contributor missing from manifest",
        ));
    }
    Ok(())
}

fn verify_present_ids(
    transaction: &Connection,
    supplied: &BTreeSet<i64>,
) -> Result<(), RebuildError> {
    let actual = query_ids_no_param(
        transaction,
        "SELECT source_file_id FROM source_files WHERE file_status='present' ORDER BY source_file_id",
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    if &actual != supplied {
        return Err(RebuildError::Cas("discovery proof is incomplete"));
    }
    Ok(())
}

fn normalized_ids(ids: &[i64]) -> Result<BTreeSet<i64>, RebuildError> {
    let set = ids.iter().copied().collect::<BTreeSet<_>>();
    if set.len() != ids.len() || set.iter().any(|id| *id <= 0) {
        return Err(RebuildError::Invalid(
            "source IDs must be unique and positive",
        ));
    }
    Ok(set)
}

fn query_ids(transaction: &Connection, sql: &str, value: i64) -> Result<Vec<i64>, RebuildError> {
    let mut statement = transaction.prepare(sql)?;
    let rows = statement.query_map([value], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn query_ids_no_param(transaction: &Connection, sql: &str) -> Result<Vec<i64>, RebuildError> {
    let mut statement = transaction.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection
            .execute_batch(include_str!("../storage/schema/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("../storage/schema/0002_usage_ledger.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "../storage/schema/0003_normalized_token_usage.sql"
            ))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "../storage/schema/0006_subagent_agent_path.sql"
            ))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "../storage/schema/0007_usage_context_and_estimated_cost.sql"
            ))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "../storage/schema/0009_skill_usage_events.sql"
            ))
            .unwrap();
        // The compact fixture intentionally skips the full session-resilience
        // migration, but rebuild activation visibility includes Codex
        // quarantine state. Keep the required sidecar tables faithful to the
        // production schema instead of teaching production code to tolerate a
        // missing table.
        connection
            .execute_batch(
                "CREATE TABLE usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
                    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
                    primary_error_code TEXT NOT NULL CHECK (length(primary_error_code) > 0),
                    last_activity_at_ms INTEGER NOT NULL CHECK (last_activity_at_ms >= 0),
                    first_seen_at_ms INTEGER NOT NULL CHECK (first_seen_at_ms >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
                    PRIMARY KEY (ledger_epoch, root_session_id),
                    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id)
                 );
                 CREATE TABLE usage_session_quarantine_sources (
                    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
                    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
                    source_file_id INTEGER NOT NULL CHECK (source_file_id > 0),
                    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
                    device_id INTEGER NOT NULL CHECK (device_id >= 0),
                    inode INTEGER NOT NULL CHECK (inode >= 0),
                    observed_size INTEGER NOT NULL CHECK (observed_size >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
                    PRIMARY KEY (ledger_epoch, root_session_id, source_file_id),
                    FOREIGN KEY (ledger_epoch, root_session_id)
                        REFERENCES usage_session_quarantine(ledger_epoch, root_session_id)
                        ON DELETE CASCADE,
                    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id)
                 );",
            )
            .unwrap();
        // Promote the compact fixture to the v11 source-aware canonical
        // columns. The real migration performs this transformation before
        // runtime code executes; this local fixture keeps legacy insert
        // statements readable while mirroring their Codex source identity.
        connection
            .execute_batch(
                "ALTER TABLE usage_events ADD COLUMN source TEXT;
                 ALTER TABLE usage_events ADD COLUMN source_epoch INTEGER;
                 ALTER TABLE usage_event_occurrences ADD COLUMN source TEXT;
                 CREATE TABLE source_usage_epochs(
                    source TEXT PRIMARY KEY, active_epoch INTEGER NOT NULL,
                    build_epoch INTEGER, active_parser_version INTEGER NOT NULL,
                    build_parser_version INTEGER
                 );
                 INSERT INTO source_usage_epochs(source,active_epoch,active_parser_version)
                    VALUES ('codex',0,0);
                 CREATE TRIGGER usage_events_v11_defaults AFTER INSERT ON usage_events
                 WHEN NEW.source IS NULL
                 BEGIN
                    UPDATE usage_events SET source='codex',source_epoch=NEW.ledger_epoch
                    WHERE rowid=NEW.rowid;
                 END;
                 CREATE TRIGGER usage_occurrences_v11_defaults AFTER INSERT ON usage_event_occurrences
                 WHEN NEW.source IS NULL
                 BEGIN
                    UPDATE usage_event_occurrences SET source='codex' WHERE rowid=NEW.rowid;
                 END;
                 CREATE TRIGGER app_meta_epoch_v11 AFTER UPDATE OF usage_active_epoch,
                    usage_build_epoch,usage_parser_version,usage_build_parser_version ON app_meta
                 BEGIN
                    UPDATE source_usage_epochs SET active_epoch=NEW.usage_active_epoch,
                      build_epoch=NEW.usage_build_epoch,
                      active_parser_version=NEW.usage_parser_version,
                      build_parser_version=NEW.usage_build_parser_version
                    WHERE source='codex';
                 END;
                 ALTER TABLE threads ADD COLUMN source TEXT NOT NULL DEFAULT 'codex';
                 ALTER TABLE threads ADD COLUMN native_session_id TEXT NOT NULL DEFAULT '';",
            )
            .unwrap();
        connection
    }

    fn thread(connection: &Connection, id: &str) {
        connection
            .execute(
                "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,
                archived,metadata_quality_status,metadata_resolved_at_ms)
             VALUES (?1,'codex',?1,NULL,?1,'main',0,'complete',1)",
                [id],
            )
            .unwrap();
    }

    fn source(connection: &Connection, id: i64, thread_id: &str, size: i64, status: &str) {
        connection.execute(
            "INSERT INTO source_files(source_file_id,thread_id,current_path,source_area,
                device_id,inode,file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms)
             VALUES (?1,?2,?3,'sessions',?1,?1,1,?4,1,?5,1)",
            params![id,thread_id,format!("/sessions/{id}.jsonl"),size,status],
        ).unwrap();
    }

    fn active_state(connection: &Connection, source_id: i64, size: i64) {
        active_state_with_versions(connection, source_id, size, 2, 2);
    }

    fn active_state_with_versions(
        connection: &Connection,
        source_id: i64,
        size: i64,
        parser_version: i64,
        canonical_algorithm_version: i64,
    ) {
        connection
            .execute(
                "INSERT INTO source_checkpoints(source_file_id,consumer_kind,parser_version,
                committed_offset,guard_hash,processing_status)
                VALUES (?1,'usage',?2,?3,X'01','ready')",
                params![source_id, parser_version, size],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_source_states(ledger_epoch,source_file_id,file_generation,
                device_id,inode,usage_parser_version,canonical_algorithm_version,
                resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,
                owning_thread_id,root_session_id,continuation_state,chain_state,
                chain_block_reason,updated_at_ms)
             VALUES (1,?1,1,?1,?1,?2,?3,?4,?4,'none',NULL,'root','root',
                     'owning_live','continuous',NULL,1)",
                params![source_id, parser_version, canonical_algorithm_version, size],
            )
            .unwrap();
    }

    fn visible_usage_event(connection: &Connection, epoch: i64, model: &str) {
        connection
            .execute(
                "INSERT INTO usage_events(
                    ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                    turn_key,model,input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                    reasoning_tokens,total_tokens,quality_status,source_file_id,file_generation,
                    source_start_offset,source_end_offset,created_at_ms,reasoning_effort,
                    estimated_cost_nanos_usd,source,source_epoch)
                 VALUES (?1,'visible-event','normal',10,'root','root','turn-1',?2,
                         10,1,0,5,1,15,'complete',1,1,0,10,10,'medium',100,'codex',?1)",
                params![epoch, model],
            )
            .unwrap();
    }

    fn prepare_visible_rebuild(connection: &mut Connection, build_model: &str) {
        thread(connection, "root");
        source(connection, 1, "root", 100, "present");
        let parser = crate::usage::USAGE_PARSER_VERSION;
        let canonical = crate::usage::canonical_algorithm_for(parser).unwrap();
        connection
            .execute(
                "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1 WHERE id=1",
                [parser],
            )
            .unwrap();
        active_state_with_versions(connection, 1, 100, parser, canonical);
        visible_usage_event(connection, 1, "gpt-visible");

        let snapshot = RebuildLedger::new(connection)
            .begin_or_resume(parser, &[1], 1)
            .unwrap();
        assert_eq!((snapshot.active_epoch, snapshot.build_epoch), (1, 2));
        assert_eq!(
            RebuildLedger::new(connection)
                .record_progress(progress(1, 0, 100, 100, TailProof::None))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        visible_usage_event(connection, 2, build_model);
    }

    fn progress(source: i64, start: u64, end: u64, size: u64, tail: TailProof) -> SourceProgress {
        SourceProgress {
            source_file_id: source,
            expected_generation: 1,
            start_offset: start,
            last_complete_offset: end,
            observed_raw_size: size,
            expected_guard_hash: (start > 0).then(|| vec![1]),
            guard_hash: (end > 0).then(|| vec![1]),
            tail,
            updated_at_ms: 10,
        }
    }

    #[test]
    fn fresh_build_freezes_active_and_present_members_without_touching_old_epoch() {
        let mut connection = database();
        thread(&connection, "root");
        source(&connection, 1, "root", 100, "missing");
        source(&connection, 2, "root", 80, "present");
        connection
            .execute(
                "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=2 WHERE id=1",
                [],
            )
            .unwrap();
        active_state(&connection, 1, 100);

        let snapshot = RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[2], 10)
            .unwrap();
        assert_eq!((snapshot.active_epoch, snapshot.build_epoch), (1, 2));
        assert_eq!(snapshot.members.len(), 2);
        assert_eq!(snapshot.members[0].membership_reason, "active_contributor");
        assert_eq!(
            snapshot.members[0].completion_status,
            CompletionStatus::Blocked
        );
        assert_eq!(snapshot.members[0].required_through_offset, 100);
        assert_eq!(
            snapshot.members[1].membership_reason,
            "present_at_build_start"
        );
        assert_eq!(
            snapshot.members[1].completion_status,
            CompletionStatus::Pending
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_source_states WHERE ledger_epoch=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(connection.query_row(
            "SELECT processing_status FROM source_checkpoints WHERE source_file_id=2 AND consumer_kind='usage'",
            [], |row| row.get::<_,String>(0)).unwrap(), "rebuild_required");

        let resumed = RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[2], 11)
            .unwrap();
        assert_eq!(
            resumed, snapshot,
            "restart resumes the frozen manifest idempotently"
        );
        assert!(
            RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION + 1, &[2], 12)
                .is_err()
        );
    }

    #[test]
    fn bounded_progress_resumes_nonzero_and_activation_waits_for_every_member() {
        let mut connection = database();
        thread(&connection, "root");
        source(&connection, 1, "root", 100, "present");
        source(&connection, 2, "root", 80, "present");
        RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 1)
            .unwrap();
        let ledger = &mut RebuildLedger::new(&mut connection);
        assert_eq!(
            ledger
                .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
                .unwrap(),
            ProgressOutcome::Advanced
        );
        assert_eq!(
            ledger
                .record_progress(progress(1, 50, 100, 100, TailProof::None))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        assert!(ledger.activate(&[1, 2]).is_err());
        let app_before: (i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(app_before, (0, Some(1)));

        let ledger = &mut RebuildLedger::new(&mut connection);
        ledger.block_source(2, "RETRYABLE", 2).unwrap();
        ledger.retry_blocked(2, 3).unwrap();
        assert_eq!(
            ledger
                .record_progress(progress(
                    2,
                    0,
                    70,
                    80,
                    TailProof::HalfLine { start_offset: 70 },
                ))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        let activated = ledger.activate(&[1, 2]).unwrap();
        assert_eq!(
            activated,
            ActivationOutcome {
                active_epoch: 1,
                data_revision: 1
            }
        );
        let app: (i64, Option<i64>, i64) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(app, (1, None, crate::usage::USAGE_PARSER_VERSION));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_build_sources", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_cas_preserves_old_epoch_and_progress_retry_is_idempotent() {
        let mut connection = database();
        thread(&connection, "root");
        source(&connection, 1, "root", 100, "present");
        source(&connection, 2, "root", 40, "missing");
        RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1], 1)
            .unwrap();
        let ledger = &mut RebuildLedger::new(&mut connection);
        let bad = progress(1, 10, 100, 100, TailProof::None);
        assert!(ledger.record_progress(bad).is_err());
        assert_eq!(connection.query_row(
            "SELECT committed_offset FROM source_checkpoints WHERE source_file_id=1 AND consumer_kind='usage'",
            [], |row| row.get::<_,i64>(0)).unwrap(),0);
        let good = progress(1, 0, 100, 100, TailProof::None);
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(good.clone())
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(good)
                .unwrap(),
            ProgressOutcome::AlreadyApplied
        );
        connection
            .execute(
                "UPDATE source_files SET file_status='present' WHERE source_file_id=2",
                [],
            )
            .unwrap();
        assert!(
            RebuildLedger::new(&mut connection)
                .activate(&[1, 2])
                .is_err(),
            "new present source is not silently omitted"
        );
        let state: (i64, Option<i64>, i64) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,(SELECT data_revision FROM app_meta WHERE id=1)
             FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (0, Some(1), 0));

        let snapshot = RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 20)
            .unwrap();
        assert_eq!(snapshot.members.len(), 2);
        assert_eq!(
            snapshot.members[0].completion_status,
            CompletionStatus::Rebuilt
        );
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(progress(2, 0, 40, 40, TailProof::None))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        let activated = RebuildLedger::new(&mut connection)
            .activate(&[1, 2])
            .unwrap();
        assert_eq!(activated.active_epoch, 1);
    }
    #[test]
    fn identical_active_and_build_activation_keeps_data_revision_stable() {
        let mut connection = database();
        prepare_visible_rebuild(&mut connection, "gpt-visible");
        let before_revision: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();

        let activated = RebuildLedger::new(&mut connection).activate(&[1]).unwrap();

        assert_eq!(activated.active_epoch, 2);
        assert_eq!(activated.data_revision, before_revision);
        let state: (i64, Option<i64>, i64) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,(SELECT data_revision FROM app_meta WHERE id=1)
                 FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (2, None, before_revision));
    }

    #[test]
    fn changed_active_and_build_activation_bumps_data_revision_once() {
        let mut connection = database();
        prepare_visible_rebuild(&mut connection, "gpt-changed");
        let before_revision: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();

        let activated = RebuildLedger::new(&mut connection).activate(&[1]).unwrap();

        assert_eq!(activated.active_epoch, 2);
        assert_eq!(activated.data_revision, before_revision + 1);
        let revision: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(revision, before_revision + 1);
    }

    #[test]
    fn parser_target_replacement_keeps_manifest_and_resets_every_build_member() {
        let mut connection = database();
        thread(&connection, "root");
        source(&connection, 1, "root", 100, "present");
        source(&connection, 2, "root", 80, "present");

        RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 1)
            .unwrap();
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
                .unwrap(),
            ProgressOutcome::Advanced
        );
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(progress(2, 0, 80, 80, TailProof::None))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );

        // A pre-normalized legacy build target is durable input evidence only;
        // the current parser never starts a new canonical write for parser 2.
        connection
            .execute(
                "UPDATE source_usage_epochs SET build_parser_version=2 WHERE source='codex'",
                [],
            )
            .unwrap();

        let replaced = RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 2)
            .unwrap();
        assert_eq!(replaced.build_epoch, 1);
        assert_eq!(
            replaced.target_parser_version,
            crate::usage::USAGE_PARSER_VERSION
        );
        assert_eq!(
            replaced
                .members
                .iter()
                .map(|member| member.source_file_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            replaced
                .members
                .iter()
                .all(|member| member.completion_status == CompletionStatus::Pending)
        );

        let checkpoints = connection
            .prepare(
                "SELECT source_file_id,parser_version,committed_offset,processing_status
                 FROM source_checkpoints
                 WHERE consumer_kind='usage' ORDER BY source_file_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            checkpoints,
            vec![
                (
                    1,
                    crate::usage::USAGE_PARSER_VERSION,
                    0,
                    "rebuild_required".into()
                ),
                (
                    2,
                    crate::usage::USAGE_PARSER_VERSION,
                    0,
                    "rebuild_required".into()
                ),
            ]
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_source_states WHERE ledger_epoch=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "parser replacement must discard old-target build-only state"
        );
    }

    #[test]
    fn replacement_preserves_unaffected_build_progress_and_old_manifest_members() {
        let mut connection = database();
        thread(&connection, "root");
        source(&connection, 1, "root", 100, "present");
        source(&connection, 2, "root", 80, "present");
        source(&connection, 3, "root", 60, "present");
        RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2, 3], 1)
            .unwrap();

        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
                .unwrap(),
            ProgressOutcome::Advanced
        );
        assert_eq!(
            RebuildLedger::new(&mut connection)
                .record_progress(progress(2, 0, 80, 80, TailProof::None))
                .unwrap(),
            ProgressOutcome::Rebuilt
        );
        // Member 3 becomes missing while still unfinished. It must remain in
        // the replacement membership and stay blocked rather than disappear.
        connection
            .execute(
                "UPDATE source_files SET file_status='missing' WHERE source_file_id=3",
                [],
            )
            .unwrap();
        RebuildLedger::new(&mut connection)
            .block_source(3, "OLD_BUILD_MEMBER_MISSING", 2)
            .unwrap();

        let before2: (String, i64, String, i64, String) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=2",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        let before3: (String, i64) = connection
            .query_row(
                "SELECT completion_status,required_through_offset
                 FROM usage_build_sources WHERE build_epoch=1 AND source_file_id=3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        {
            let tx = connection.transaction().unwrap();
            replace_build_preserving_all_members_tx(
                &tx,
                0,
                1,
                crate::usage::USAGE_PARSER_VERSION,
                &[1, 2].into_iter().collect(),
                &[1].into_iter().collect(),
                3,
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let ids = connection
            .prepare("SELECT source_file_id FROM usage_build_sources WHERE build_epoch=1 ORDER BY source_file_id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec![1, 2, 3]);

        let affected: (String, i64, String, i64) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,c.committed_offset
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            affected,
            ("pending".into(), 50, "rebuild_required".into(), 0)
        );

        let after2: (String, i64, String, i64, String) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=2",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        let after3: (String, i64) = connection
            .query_row(
                "SELECT completion_status,required_through_offset
                 FROM usage_build_sources WHERE build_epoch=1 AND source_file_id=3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            after2, before2,
            "safe rebuilt member progress must be byte-for-byte preserved"
        );
        assert_eq!(
            after3, before3,
            "old missing member must remain blocked and retain proof"
        );
    }
}
