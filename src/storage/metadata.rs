//! Atomic metadata fact, checkpoint, normalized Thread, and usage-reconcile writes.
//!
//! SQL stays private behind `Ledger`. The metadata commit transaction also
//! performs the Spec04 active-usage root reconciliation and any required
//! shadow-build replacement before it commits.

use crate::domain::{
    AgentPathProvenance, AgentRole, AgentRoleProvenance, CommitOutcome, ContinuationState,
    CwdProvenance, ExistingThreadProjection, FactQualityStatus, FileStatus,
    MetadataCheckpointAdvance, MetadataCheckpointState, MetadataCommitBatch, MetadataQualityStatus,
    MetadataSourceCommit, MetadataThreadCommit, ParentHintProvenance, Patch, ProjectKind,
    ResolvedThreadPatch, RolloutMetadataFact, SourceArea, SourceFileState,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{Ledger, Result as StorageResult, StorageError};

/// Commit a batch one Thread group at a time.
///
/// A group is the unit of isolation: every source binding, safe fact,
/// metadata checkpoint, and optional normalized Thread patch in that group is
/// committed by one `BEGIN IMMEDIATE` transaction.  Groups which precede a
/// later failing group remain committed by design.
impl Ledger {
    /// Read all normalized Thread rows as a resolver projection from one
    /// SQLite snapshot.  This method does not expose SQL rows or any source
    /// payload columns.
    pub fn load_existing_threads(&self) -> StorageResult<Vec<ExistingThreadProjection>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut statement = transaction.prepare(
            "SELECT
                thread_id, parent_thread_id, root_session_id, agent_role,
                title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, current_rollout_path,
                metadata_quality_status
             FROM threads ORDER BY thread_id",
        )?;
        let mut rows = statement.query([])?;
        let mut projections = Vec::new();
        while let Some(row) = rows.next()? {
            let agent_role: String = row.get(3)?;
            let agent_role =
                AgentRole::try_from(agent_role.as_str()).map_err(super::to_domain_sql_error)?;
            let archived: i64 = row.get(11)?;
            let archived = match archived {
                0 => false,
                1 => true,
                other => {
                    return Err(StorageError::invalid_state(format!(
                        "invalid archived value {other}"
                    )));
                }
            };
            let quality: String = row.get(13)?;
            let metadata_quality_status = MetadataQualityStatus::try_from(quality.as_str())
                .map_err(super::to_domain_sql_error)?;
            let project_kind: String = row.get(7)?;
            let project_kind =
                ProjectKind::try_from(project_kind.as_str()).map_err(super::to_domain_sql_error)?;
            projections.push(ExistingThreadProjection {
                thread_id: row.get(0)?,
                parent_thread_id: row.get(1)?,
                root_session_id: row.get(2)?,
                agent_role,
                title: row.get(4)?,
                project_name: row.get(5)?,
                project_path: row.get(6)?,
                project_kind,
                metadata_model: row.get(8)?,
                created_at_ms: row.get(9)?,
                updated_at_ms: row.get(10)?,
                archived,
                current_rollout_path: row.get(12)?,
                metadata_quality_status,
            });
        }
        drop(rows);
        drop(statement);
        transaction.commit()?;
        Ok(projections)
    }

    pub fn commit_metadata(&self, batch: MetadataCommitBatch) -> StorageResult<CommitOutcome> {
        batch
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;

        let mut connection = self.connection()?;
        let mut data_changed = false;
        let mut data_revision = None;

        for group in &batch.groups {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_source_ready(&transaction, self)?;

            let changed = commit_group(&transaction, group)?;
            if changed {
                data_changed = true;
            }

            transaction.commit()?;
            let (revision, status_revision): (i64, i64) = connection.query_row(
                "SELECT data_revision,status_revision FROM app_meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if changed {
                self.publish_revisions(revision, status_revision);
            }
            if revision < 0 {
                return Err(StorageError::invalid_state(
                    "app_meta.data_revision must be non-negative".to_owned(),
                ));
            }
            data_revision = Some(revision);
        }

        let data_revision = data_revision
            .ok_or_else(|| StorageError::invalid_state("metadata commit contained no groups"))?;
        CommitOutcome::new(batch.groups.len(), data_revision, data_changed)
            .map_err(|error| StorageError::invalid_state(error.to_string()))
    }
}

fn commit_group(
    transaction: &Transaction<'_>,
    group: &MetadataThreadCommit,
) -> StorageResult<bool> {
    let current_revision = read_data_revision(transaction)?;
    let current_thread = read_thread(transaction, &group.thread_id)?;

    // Resolve and validate the patch before changing any source rows.  This
    // keeps malformed role/root combinations from ever reaching the database,
    // while the surrounding transaction still makes the operation atomic.
    let next_thread = group
        .resolved_patch
        .as_ref()
        .map(|patch| match current_thread.as_ref() {
            Some(current) => apply_existing_patch(current, patch),
            None => apply_new_patch(patch),
        })
        .transpose()?;

    // Read and validate every source precondition first.  A later source
    // failure must not leave an earlier source in this group bound or advanced.
    let mut sources = Vec::with_capacity(group.sources.len());
    for source_commit in &group.sources {
        let source = read_source(transaction, source_commit.source_file_id)?;
        validate_source_commit(transaction, group, source_commit, &source)?;
        sources.push(source);
    }

    let binding_changed_source_ids = group
        .sources
        .iter()
        .zip(sources.iter())
        .filter_map(|(source_commit, source)| {
            (source.thread_id.as_deref() != Some(group.thread_id.as_str()))
                .then_some(source_commit.source_file_id)
        })
        .collect::<Vec<_>>();

    for (source_commit, source) in group.sources.iter().zip(sources.iter()) {
        bind_source(transaction, group, source_commit, source)?;
        write_fact(transaction, &source_commit.safe_fact)?;
        write_checkpoint(
            transaction,
            source_commit.source_file_id,
            &source_commit.metadata_checkpoint_advance,
        )?;
    }

    if let (Some(patch), Some(next_thread)) = (&group.resolved_patch, next_thread.as_ref()) {
        write_thread(transaction, next_thread, current_thread.is_some())?;
        super::usage::reconcile_usage_metadata_change(
            transaction,
            &group.thread_id,
            current_thread
                .as_ref()
                .and_then(|thread| thread.root_session_id.as_deref()),
            next_thread.root_session_id.as_deref(),
            &binding_changed_source_ids,
        )?;
        verify_patch_postcondition(transaction, patch, next_thread)?;
    } else if !binding_changed_source_ids.is_empty() {
        // A source can become bound to an already-existing Thread without a
        // Thread patch.  Spec04 still requires the frozen build binding proof
        // to be reconciled in this same metadata transaction.
        let root = current_thread
            .as_ref()
            .and_then(|thread| thread.root_session_id.as_deref());
        super::usage::reconcile_usage_metadata_change(
            transaction,
            &group.thread_id,
            root,
            root,
            &binding_changed_source_ids,
        )?;
    }

    // The revision is global, but each Thread group may increase it at most
    // once.  Source binding/fact/checkpoint-only changes never increment it.
    let stable_changed = group
        .resolved_patch
        .as_ref()
        .zip(next_thread.as_ref())
        .is_some_and(|(_, next)| current_thread.as_ref() != Some(next));
    if stable_changed {
        let next_revision = current_revision
            .checked_add(1)
            .ok_or_else(|| StorageError::invalid_state("data_revision overflow"))?;
        let changed = transaction.execute(
            "UPDATE app_meta SET data_revision = ?1 WHERE id = 1 AND data_revision = ?2",
            params![next_revision, current_revision],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "app_meta row id=1 disappeared while committing metadata".to_owned(),
            ));
        }
    }

    // A write-after-read check protects the key cross-table equalities from
    // schema changes and direct SQL interference.  It is still inside the
    // transaction, so any mismatch rolls the entire group back.
    for source_commit in &group.sources {
        verify_source_postcondition(transaction, group, source_commit)?;
    }

    Ok(stable_changed)
}

pub(super) fn ensure_source_ready(
    transaction: &Transaction<'_>,
    ledger: &Ledger,
) -> StorageResult<()> {
    let (fingerprint, status): (Option<String>, String) = transaction
        .query_row(
            "SELECT codex_home_fingerprint, source_binding_status
             FROM app_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StorageError::from)?;

    match status.as_str() {
        "ready" => match fingerprint {
            Some(expected) if expected == ledger.expected_codex_home_fingerprint() => Ok(()),
            Some(expected) => Err(StorageError::source_changed(
                expected,
                ledger.expected_codex_home_fingerprint().to_owned(),
            )),
            None => Err(StorageError::invalid_state(
                "ready CODEX_HOME binding has no fingerprint",
            )),
        },
        "unbound" => Err(StorageError::source_unbound()),
        "source_changed" => Err(StorageError::source_changed(
            fingerprint.unwrap_or_default(),
            ledger.expected_codex_home_fingerprint().to_owned(),
        )),
        other => Err(StorageError::invalid_state(format!(
            "unknown source binding status {other:?}"
        ))),
    }
}

fn read_data_revision(transaction: &Transaction<'_>) -> StorageResult<i64> {
    let revision: i64 = transaction.query_row(
        "SELECT data_revision FROM app_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if revision < 0 {
        return Err(StorageError::invalid_state(
            "app_meta.data_revision must be non-negative".to_owned(),
        ));
    }
    Ok(revision)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThreadRow {
    thread_id: String,
    parent_thread_id: Option<String>,
    root_session_id: Option<String>,
    agent_role: AgentRole,
    title: Option<String>,
    project_name: Option<String>,
    project_path: Option<String>,
    project_kind: ProjectKind,
    metadata_model: Option<String>,
    created_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    archived: bool,
    current_rollout_path: Option<String>,
    metadata_quality_status: MetadataQualityStatus,
    metadata_resolved_at_ms: i64,
}

fn read_thread(transaction: &Transaction<'_>, thread_id: &str) -> StorageResult<Option<ThreadRow>> {
    transaction
        .query_row(
            "SELECT
                thread_id, parent_thread_id, root_session_id, agent_role,
                title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, current_rollout_path,
                metadata_quality_status, metadata_resolved_at_ms
             FROM threads WHERE thread_id = ?1",
            [thread_id],
            |row| {
                let agent_role: String = row.get(3)?;
                let agent_role =
                    AgentRole::try_from(agent_role.as_str()).map_err(super::to_domain_sql_error)?;
                let archived: i64 = row.get(11)?;
                let archived = match archived {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "invalid archived value {other}"
                        )));
                    }
                };
                let quality: String = row.get(13)?;
                let quality = MetadataQualityStatus::try_from(quality.as_str())
                    .map_err(super::to_domain_sql_error)?;
                let project_kind: String = row.get(7)?;
                let project_kind = ProjectKind::try_from(project_kind.as_str())
                    .map_err(super::to_domain_sql_error)?;
                Ok(ThreadRow {
                    thread_id: row.get(0)?,
                    parent_thread_id: row.get(1)?,
                    root_session_id: row.get(2)?,
                    agent_role,
                    title: row.get(4)?,
                    project_name: row.get(5)?,
                    project_path: row.get(6)?,
                    project_kind,
                    metadata_model: row.get(8)?,
                    created_at_ms: row.get(9)?,
                    updated_at_ms: row.get(10)?,
                    archived,
                    current_rollout_path: row.get(12)?,
                    metadata_quality_status: quality,
                    metadata_resolved_at_ms: row.get(14)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn read_source(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> StorageResult<SourceFileState> {
    let source = transaction
        .query_row(
            "SELECT
                source_file_id, thread_id, current_path, source_area,
                device_id, inode, file_generation, observed_size,
                observed_mtime_ns, file_status, last_seen_at_ms
             FROM source_files WHERE source_file_id = ?1",
            [source_file_id],
            |row| {
                let area: String = row.get(3)?;
                let area =
                    SourceArea::try_from(area.as_str()).map_err(super::to_domain_sql_error)?;
                let status: String = row.get(9)?;
                let status =
                    FileStatus::try_from(status.as_str()).map_err(super::to_domain_sql_error)?;
                SourceFileState::new(
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                    area,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    status,
                    row.get(10)?,
                )
                .map_err(super::to_domain_sql_error)
            },
        )
        .optional()?;
    source.ok_or_else(|| {
        StorageError::invalid_state(format!("source_file_id {source_file_id} does not exist"))
    })
}

fn read_checkpoint(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> StorageResult<Option<MetadataCheckpointState>> {
    transaction
        .query_row(
            "SELECT parser_version, committed_offset, guard_hash,
                    processing_status, last_successful_scan_at_ms, last_error_code
             FROM source_checkpoints
             WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
            [source_file_id],
            |row| {
                let processing_status: String = row.get(3)?;
                let processing_status =
                    crate::domain::CheckpointProcessingStatus::try_from(processing_status.as_str())
                        .map_err(super::to_domain_sql_error)?;
                MetadataCheckpointState::new(
                    source_file_id,
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    processing_status,
                    row.get(4)?,
                    row.get(5)?,
                )
                .map_err(super::to_domain_sql_error)
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn read_fact(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> StorageResult<Option<RolloutMetadataFact>> {
    transaction
        .query_row(
            "SELECT
                source_file_id, file_generation, metadata_parser_version,
                resolved_through_offset, owning_thread_id, continuation_state,
                cwd, cwd_provenance, cwd_record_offset, created_at_ms,
                latest_context_model, latest_context_at_ms,
                parent_thread_id_hint, parent_hint_provenance,
                parent_hint_record_offset, agent_role_hint,
                agent_role_provenance, agent_role_record_offset,
                agent_path, agent_path_provenance, agent_path_record_offset,
                replay_start_offset, owning_records_start_offset,
                ownership_confidence, fact_quality_status, updated_at_ms,
                latest_context_turn_id, relationship_conflict
             FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_file_id],
            |row| {
                let continuation: String = row.get(5)?;
                let continuation = ContinuationState::try_from(continuation.as_str())
                    .map_err(super::to_domain_sql_error)?;
                let cwd_provenance: Option<String> = row.get(7)?;
                let cwd_provenance = cwd_provenance
                    .as_deref()
                    .map(CwdProvenance::try_from)
                    .transpose()
                    .map_err(super::to_domain_sql_error)?;
                let parent_provenance: Option<String> = row.get(13)?;
                let parent_provenance = parent_provenance
                    .as_deref()
                    .map(ParentHintProvenance::try_from)
                    .transpose()
                    .map_err(super::to_domain_sql_error)?;
                let role_provenance: Option<String> = row.get(16)?;
                let role_provenance = role_provenance
                    .as_deref()
                    .map(AgentRoleProvenance::try_from)
                    .transpose()
                    .map_err(super::to_domain_sql_error)?;
                let agent_path_provenance: Option<String> = row.get(19)?;
                let agent_path_provenance = agent_path_provenance
                    .as_deref()
                    .map(AgentPathProvenance::try_from)
                    .transpose()
                    .map_err(super::to_domain_sql_error)?;
                let ownership: String = row.get(23)?;
                let ownership = crate::domain::OwnershipConfidence::try_from(ownership.as_str())
                    .map_err(super::to_domain_sql_error)?;
                let quality: String = row.get(24)?;
                let quality = FactQualityStatus::try_from(quality.as_str())
                    .map_err(super::to_domain_sql_error)?;
                let relationship_conflict: i64 = row.get(27)?;
                let relationship_conflict = match relationship_conflict {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "invalid relationship_conflict value {other}"
                        )));
                    }
                };
                let fact = RolloutMetadataFact {
                    source_file_id: row.get(0)?,
                    file_generation: row.get(1)?,
                    metadata_parser_version: row.get(2)?,
                    resolved_through_offset: row.get(3)?,
                    owning_thread_id: row.get(4)?,
                    continuation_state: continuation,
                    cwd: row.get(6)?,
                    cwd_provenance,
                    cwd_record_offset: row.get(8)?,
                    created_at_ms: row.get(9)?,
                    latest_context_model: row.get(10)?,
                    latest_context_at_ms: row.get(11)?,
                    latest_context_turn_id: row.get(26)?,
                    parent_thread_id_hint: row.get(12)?,
                    parent_hint_provenance: parent_provenance,
                    parent_hint_record_offset: row.get(14)?,
                    agent_role_hint: row.get(15)?,
                    agent_role_provenance: role_provenance,
                    agent_role_record_offset: row.get(17)?,
                    agent_path: row.get(18)?,
                    agent_path_provenance,
                    agent_path_record_offset: row.get(20)?,
                    replay_start_offset: row.get(21)?,
                    owning_records_start_offset: row.get(22)?,
                    ownership_confidence: ownership,
                    fact_quality_status: quality,
                    updated_at_ms: row.get(25)?,
                    relationship_conflict,
                };
                fact.validate().map_err(super::to_domain_sql_error)?;
                Ok(fact)
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn validate_source_commit(
    transaction: &Transaction<'_>,
    group: &MetadataThreadCommit,
    source_commit: &MetadataSourceCommit,
    source: &SourceFileState,
) -> StorageResult<()> {
    if source.file_generation != source_commit.expected_file_generation {
        return Err(StorageError::invalid_state(format!(
            "source {} generation changed (expected {}, found {})",
            source_commit.source_file_id,
            source_commit.expected_file_generation,
            source.file_generation
        )));
    }
    if source.thread_id != source_commit.expected_previous_thread_id {
        return Err(StorageError::invalid_state(format!(
            "source {} binding CAS failed",
            source_commit.source_file_id
        )));
    }
    if source.file_status != FileStatus::Present {
        return Err(StorageError::invalid_state(format!(
            "source {} is not present",
            source_commit.source_file_id
        )));
    }

    let advance = &source_commit.metadata_checkpoint_advance;
    let fact = &source_commit.safe_fact;
    if fact.source_file_id != source_commit.source_file_id
        || fact.file_generation != source_commit.expected_file_generation
        || fact.owning_thread_id != group.thread_id
        || fact.metadata_parser_version != advance.parser_version
        || fact.resolved_through_offset != advance.committed_offset
    {
        return Err(StorageError::invalid_state(format!(
            "source {} fact/checkpoint identity or offset mismatch",
            source_commit.source_file_id
        )));
    }

    if advance.committed_offset > source.observed_size {
        return Err(StorageError::invalid_state(format!(
            "source {} metadata offset exceeds observed size",
            source_commit.source_file_id
        )));
    }
    if advance.committed_offset > 0
        && !matches!(
            fact.continuation_state,
            ContinuationState::ReplayedAncestor | ContinuationState::OwningLive
        )
    {
        return Err(StorageError::invalid_state(format!(
            "source {} cannot continue from a non-resumable fact",
            source_commit.source_file_id
        )));
    }
    validate_fact_offsets(fact)?;

    let bound_source = SourceFileState {
        thread_id: Some(group.thread_id.clone()),
        ..source.clone()
    };
    let checkpoint = MetadataCheckpointState::new(
        source_commit.source_file_id,
        advance.parser_version,
        advance.committed_offset,
        advance.guard_hash.clone(),
        advance.processing_status,
        advance.last_successful_scan_at_ms,
        advance.last_error_code.clone(),
    )
    .map_err(|error| StorageError::invalid_state(error.to_string()))?;
    fact.validate_against(&bound_source, &checkpoint)
        .map_err(|error| StorageError::invalid_state(error.to_string()))?;

    if let Some(current) = read_checkpoint(transaction, source_commit.source_file_id)? {
        current
            .validate_against(source)
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        if advance.parser_version < current.parser_version
            || (advance.parser_version == current.parser_version
                && advance.committed_offset < current.committed_offset)
        {
            return Err(StorageError::invalid_state(format!(
                "source {} metadata checkpoint regresses",
                source_commit.source_file_id
            )));
        }
    }
    Ok(())
}

fn validate_fact_offsets(fact: &RolloutMetadataFact) -> StorageResult<()> {
    let fields = [
        ("cwd_record_offset", fact.cwd_record_offset),
        ("parent_hint_record_offset", fact.parent_hint_record_offset),
        ("agent_role_record_offset", fact.agent_role_record_offset),
        ("agent_path_record_offset", fact.agent_path_record_offset),
        ("replay_start_offset", fact.replay_start_offset),
        (
            "owning_records_start_offset",
            fact.owning_records_start_offset,
        ),
    ];
    if let Some((field, _offset)) = fields
        .into_iter()
        .find(|(_, offset)| offset.is_some_and(|offset| offset > fact.resolved_through_offset))
    {
        return Err(StorageError::invalid_state(format!(
            "{field} exceeds resolved_through_offset"
        )));
    }
    Ok(())
}

fn bind_source(
    transaction: &Transaction<'_>,
    group: &MetadataThreadCommit,
    source_commit: &MetadataSourceCommit,
    source: &SourceFileState,
) -> StorageResult<()> {
    let changed = transaction.execute(
        "UPDATE source_files
         SET thread_id = ?1
         WHERE source_file_id = ?2
           AND file_generation = ?3
           AND thread_id IS ?4",
        params![
            group.thread_id,
            source_commit.source_file_id,
            source_commit.expected_file_generation,
            source.thread_id.as_deref(),
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(format!(
            "source {} binding CAS failed while writing",
            source_commit.source_file_id
        )));
    }
    Ok(())
}

fn write_fact(transaction: &Transaction<'_>, fact: &RolloutMetadataFact) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO rollout_metadata_facts (
            source_file_id, file_generation, metadata_parser_version,
            resolved_through_offset, owning_thread_id, continuation_state,
            cwd, cwd_provenance, cwd_record_offset, created_at_ms,
            latest_context_model, latest_context_at_ms,
            parent_thread_id_hint, parent_hint_provenance,
            parent_hint_record_offset, agent_role_hint,
            agent_role_provenance, agent_role_record_offset,
            agent_path, agent_path_provenance, agent_path_record_offset,
            replay_start_offset, owning_records_start_offset,
            ownership_confidence, fact_quality_status, updated_at_ms,
            latest_context_turn_id, relationship_conflict
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
            ?24, ?25, ?26, ?27, ?28
         )
         ON CONFLICT(source_file_id) DO UPDATE SET
            file_generation = excluded.file_generation,
            metadata_parser_version = excluded.metadata_parser_version,
            resolved_through_offset = excluded.resolved_through_offset,
            owning_thread_id = excluded.owning_thread_id,
            continuation_state = excluded.continuation_state,
            cwd = excluded.cwd,
            cwd_provenance = excluded.cwd_provenance,
            cwd_record_offset = excluded.cwd_record_offset,
            created_at_ms = excluded.created_at_ms,
            latest_context_model = excluded.latest_context_model,
            latest_context_at_ms = excluded.latest_context_at_ms,
            parent_thread_id_hint = excluded.parent_thread_id_hint,
            parent_hint_provenance = excluded.parent_hint_provenance,
            parent_hint_record_offset = excluded.parent_hint_record_offset,
            agent_role_hint = excluded.agent_role_hint,
            agent_role_provenance = excluded.agent_role_provenance,
            agent_role_record_offset = excluded.agent_role_record_offset,
            agent_path = excluded.agent_path,
            agent_path_provenance = excluded.agent_path_provenance,
            agent_path_record_offset = excluded.agent_path_record_offset,
            replay_start_offset = excluded.replay_start_offset,
            owning_records_start_offset = excluded.owning_records_start_offset,
            ownership_confidence = excluded.ownership_confidence,
            fact_quality_status = excluded.fact_quality_status,
            updated_at_ms = excluded.updated_at_ms,
            latest_context_turn_id = excluded.latest_context_turn_id,
            relationship_conflict = excluded.relationship_conflict",
        params![
            fact.source_file_id,
            fact.file_generation,
            fact.metadata_parser_version,
            fact.resolved_through_offset,
            fact.owning_thread_id,
            fact.continuation_state.as_str(),
            fact.cwd,
            fact.cwd_provenance.map(CwdProvenance::as_str),
            fact.cwd_record_offset,
            fact.created_at_ms,
            fact.latest_context_model,
            fact.latest_context_at_ms,
            fact.parent_thread_id_hint,
            fact.parent_hint_provenance
                .map(ParentHintProvenance::as_str),
            fact.parent_hint_record_offset,
            fact.agent_role_hint,
            fact.agent_role_provenance.map(AgentRoleProvenance::as_str),
            fact.agent_role_record_offset,
            fact.agent_path,
            fact.agent_path_provenance.map(AgentPathProvenance::as_str),
            fact.agent_path_record_offset,
            fact.replay_start_offset,
            fact.owning_records_start_offset,
            fact.ownership_confidence.as_str(),
            fact.fact_quality_status.as_str(),
            fact.updated_at_ms,
            fact.latest_context_turn_id,
            if fact.relationship_conflict {
                1_i64
            } else {
                0_i64
            },
        ],
    )?;
    Ok(())
}

fn write_checkpoint(
    transaction: &Transaction<'_>,
    source_file_id: i64,
    advance: &MetadataCheckpointAdvance,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO source_checkpoints (
            source_file_id, consumer_kind, parser_version, committed_offset,
            guard_hash, processing_status, last_successful_scan_at_ms,
            last_error_code
         ) VALUES (?1, 'metadata', ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(source_file_id, consumer_kind) DO UPDATE SET
            parser_version = excluded.parser_version,
            committed_offset = excluded.committed_offset,
            guard_hash = excluded.guard_hash,
            processing_status = excluded.processing_status,
            last_successful_scan_at_ms = excluded.last_successful_scan_at_ms,
            last_error_code = excluded.last_error_code",
        params![
            source_file_id,
            advance.parser_version,
            advance.committed_offset,
            advance.guard_hash,
            advance.processing_status.as_str(),
            advance.last_successful_scan_at_ms,
            advance.last_error_code,
        ],
    )?;
    Ok(())
}

fn apply_existing_patch(
    current: &ThreadRow,
    patch: &ResolvedThreadPatch,
) -> StorageResult<ThreadRow> {
    if patch.resolved_at_ms < current.metadata_resolved_at_ms {
        return Err(StorageError::invalid_state(format!(
            "patch for Thread {} is older than metadata_resolved_at_ms",
            patch.thread_id
        )));
    }
    let next = ThreadRow {
        thread_id: current.thread_id.clone(),
        parent_thread_id: apply_optional(&current.parent_thread_id, &patch.parent_thread_id),
        root_session_id: apply_optional(&current.root_session_id, &patch.root_session_id),
        agent_role: apply_required(current.agent_role, &patch.agent_role),
        title: apply_optional(&current.title, &patch.title),
        project_name: apply_optional(&current.project_name, &patch.project_name),
        project_path: apply_optional(&current.project_path, &patch.project_path),
        project_kind: apply_required(current.project_kind, &patch.project_kind),
        metadata_model: apply_optional(&current.metadata_model, &patch.metadata_model),
        created_at_ms: apply_optional(&current.created_at_ms, &patch.created_at_ms),
        updated_at_ms: apply_optional(&current.updated_at_ms, &patch.updated_at_ms),
        archived: apply_required(current.archived, &patch.archived),
        current_rollout_path: apply_optional(
            &current.current_rollout_path,
            &patch.current_rollout_path,
        ),
        metadata_quality_status: patch.metadata_quality_status,
        metadata_resolved_at_ms: patch.resolved_at_ms,
    };
    validate_thread_row(&next)?;
    Ok(next)
}

fn apply_new_patch(patch: &ResolvedThreadPatch) -> StorageResult<ThreadRow> {
    let mut next = ThreadRow {
        thread_id: patch.thread_id.clone(),
        parent_thread_id: apply_optional(&None, &patch.parent_thread_id),
        root_session_id: apply_optional(&None, &patch.root_session_id),
        agent_role: apply_required(AgentRole::Unknown, &patch.agent_role),
        title: apply_optional(&None, &patch.title),
        project_name: apply_optional(&None, &patch.project_name),
        project_path: apply_optional(&None, &patch.project_path),
        project_kind: apply_required(ProjectKind::Unknown, &patch.project_kind),
        metadata_model: apply_optional(&None, &patch.metadata_model),
        created_at_ms: apply_optional(&None, &patch.created_at_ms),
        updated_at_ms: apply_optional(&None, &patch.updated_at_ms),
        archived: apply_required(false, &patch.archived),
        current_rollout_path: apply_optional(&None, &patch.current_rollout_path),
        metadata_quality_status: patch.metadata_quality_status,
        metadata_resolved_at_ms: patch.resolved_at_ms,
    };
    // A newly created main Thread has the schema-mandated self root.  Keep is
    // intentionally treated as the default here; an explicit root patch is
    // still checked by validate_thread_row below.
    if next.agent_role == AgentRole::Main
        && next.root_session_id.is_none()
        && patch.root_session_id.is_keep()
    {
        next.root_session_id = Some(next.thread_id.clone());
    }
    validate_thread_row(&next)?;
    Ok(next)
}

fn apply_optional<T: Clone>(current: &Option<T>, patch: &Patch<T>) -> Option<T> {
    match patch {
        Patch::Keep => current.clone(),
        Patch::Set(value) => Some(value.clone()),
        Patch::Clear => None,
    }
}

fn apply_required<T: Copy>(current: T, patch: &Patch<T>) -> T {
    match patch {
        Patch::Keep | Patch::Clear => current,
        Patch::Set(value) => *value,
    }
}

fn validate_thread_row(thread: &ThreadRow) -> StorageResult<()> {
    match thread.agent_role {
        AgentRole::Main => {
            if thread.parent_thread_id.is_some()
                || thread.root_session_id.as_deref() != Some(thread.thread_id.as_str())
            {
                return Err(StorageError::invalid_state(format!(
                    "main Thread {} must have no parent and self root",
                    thread.thread_id
                )));
            }
        }
        AgentRole::Subagent => {
            if thread.parent_thread_id.is_none() {
                return Err(StorageError::invalid_state(format!(
                    "subagent Thread {} requires a parent",
                    thread.thread_id
                )));
            }
        }
        AgentRole::Unknown => {
            if thread.root_session_id.is_some() {
                return Err(StorageError::invalid_state(format!(
                    "unknown Thread {} cannot have a root session",
                    thread.thread_id
                )));
            }
        }
    }
    if thread.metadata_resolved_at_ms < 0 {
        return Err(StorageError::invalid_state(
            "metadata_resolved_at_ms must be non-negative".to_owned(),
        ));
    }
    Ok(())
}

fn write_thread(
    transaction: &Transaction<'_>,
    thread: &ThreadRow,
    existed: bool,
) -> StorageResult<()> {
    if existed {
        let changed = transaction.execute(
            "UPDATE threads SET
                parent_thread_id = ?2,
                root_session_id = ?3,
                agent_role = ?4,
                title = ?5,
                project_name = ?6,
                project_path = ?7,
                project_kind = ?8,
                metadata_model = ?9,
                created_at_ms = ?10,
                updated_at_ms = ?11,
                archived = ?12,
                current_rollout_path = ?13,
                metadata_quality_status = ?14,
                metadata_resolved_at_ms = ?15
             WHERE thread_id = ?1",
            params![
                thread.thread_id,
                thread.parent_thread_id,
                thread.root_session_id,
                thread.agent_role.as_str(),
                thread.title,
                thread.project_name,
                thread.project_path,
                thread.project_kind.as_str(),
                thread.metadata_model,
                thread.created_at_ms,
                thread.updated_at_ms,
                if thread.archived { 1_i64 } else { 0_i64 },
                thread.current_rollout_path,
                thread.metadata_quality_status.as_str(),
                thread.metadata_resolved_at_ms,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(format!(
                "Thread {} disappeared while applying metadata patch",
                thread.thread_id
            )));
        }
    } else {
        transaction.execute(
            "INSERT INTO threads (
                thread_id, parent_thread_id, root_session_id, agent_role,
                title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, current_rollout_path,
                metadata_quality_status, metadata_resolved_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                thread.thread_id,
                thread.parent_thread_id,
                thread.root_session_id,
                thread.agent_role.as_str(),
                thread.title,
                thread.project_name,
                thread.project_path,
                thread.project_kind.as_str(),
                thread.metadata_model,
                thread.created_at_ms,
                thread.updated_at_ms,
                if thread.archived { 1_i64 } else { 0_i64 },
                thread.current_rollout_path,
                thread.metadata_quality_status.as_str(),
                thread.metadata_resolved_at_ms,
            ],
        )?;
    }
    Ok(())
}

fn verify_patch_postcondition(
    transaction: &Transaction<'_>,
    patch: &ResolvedThreadPatch,
    expected: &ThreadRow,
) -> StorageResult<()> {
    let actual = read_thread(transaction, &patch.thread_id)?.ok_or_else(|| {
        StorageError::invalid_state(format!(
            "Thread {} missing after metadata patch",
            patch.thread_id
        ))
    })?;
    if &actual != expected {
        return Err(StorageError::invalid_state(format!(
            "Thread {} patch postcondition failed",
            patch.thread_id
        )));
    }
    Ok(())
}

fn verify_source_postcondition(
    transaction: &Transaction<'_>,
    group: &MetadataThreadCommit,
    source_commit: &MetadataSourceCommit,
) -> StorageResult<()> {
    let source = read_source(transaction, source_commit.source_file_id)?;
    if source.thread_id.as_deref() != Some(group.thread_id.as_str())
        || source.file_generation != source_commit.expected_file_generation
    {
        return Err(StorageError::invalid_state(format!(
            "source {} owning-id postcondition failed",
            source_commit.source_file_id
        )));
    }
    let fact = read_fact(transaction, source_commit.source_file_id)?.ok_or_else(|| {
        StorageError::invalid_state(format!(
            "source {} fact missing after metadata write",
            source_commit.source_file_id
        ))
    })?;
    let checkpoint =
        read_checkpoint(transaction, source_commit.source_file_id)?.ok_or_else(|| {
            StorageError::invalid_state(format!(
                "source {} metadata checkpoint missing after metadata write",
                source_commit.source_file_id
            ))
        })?;
    if fact.owning_thread_id != group.thread_id
        || fact.file_generation != source.file_generation
        || fact.metadata_parser_version != checkpoint.parser_version
        || fact.resolved_through_offset != checkpoint.committed_offset
    {
        return Err(StorageError::invalid_state(format!(
            "source {} fact/checkpoint postcondition failed",
            source_commit.source_file_id
        )));
    }
    let fact_source = SourceFileState {
        thread_id: Some(group.thread_id.clone()),
        ..source
    };
    fact.validate_against(&fact_source, &checkpoint)
        .map_err(|error| StorageError::invalid_state(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::domain::{
        CheckpointProcessingStatus, FactQualityStatus, MetadataCheckpointAdvance,
        OwnershipConfidence, ProjectKind, SafeFactState, ScanStartEvent, ScanTrigger,
    };
    use crate::storage::{LedgerOptions, SourceBindingStatus};

    fn temp_paths(name: &str) -> (PathBuf, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("usagi-metadata-{name}-{stamp}"));
        fs::create_dir_all(&root).unwrap();
        (root.join("mu.sqlite3"), root.join("codex"))
    }

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-storage-metadata")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn source_fact_for(
        source_file_id: i64,
        file_generation: i64,
        offset: i64,
        owner: &str,
    ) -> RolloutMetadataFact {
        RolloutMetadataFact {
            source_file_id,
            file_generation,
            metadata_parser_version: 1,
            resolved_through_offset: offset,
            owning_thread_id: owner.to_owned(),
            continuation_state: ContinuationState::OwningLive,
            cwd: None,
            cwd_provenance: None,
            cwd_record_offset: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            parent_thread_id_hint: None,
            parent_hint_provenance: None,
            parent_hint_record_offset: None,
            agent_role_hint: None,
            agent_role_provenance: None,
            agent_role_record_offset: None,
            agent_path: None,
            agent_path_provenance: None,
            agent_path_record_offset: None,
            replay_start_offset: None,
            owning_records_start_offset: None,
            ownership_confidence: OwnershipConfidence::Confirmed,
            fact_quality_status: FactQualityStatus::Complete,
            relationship_conflict: false,
            updated_at_ms: 10,
        }
    }

    fn source_commit_for(
        source_file_id: i64,
        file_generation: i64,
        offset: i64,
        owner: &str,
        expected_previous_thread_id: Option<String>,
    ) -> MetadataSourceCommit {
        MetadataSourceCommit::new(
            source_file_id,
            file_generation,
            expected_previous_thread_id,
            owner,
            source_fact_for(source_file_id, file_generation, offset, owner),
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: offset,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap()
    }

    fn source_commit(expected_previous_thread_id: Option<String>) -> MetadataSourceCommit {
        source_commit_for(1, 1, 10, "thread", expected_previous_thread_id)
    }

    fn insert_source(ledger: &Ledger) {
        let connection = ledger.connection().unwrap();
        connection
            .execute(
                "INSERT INTO source_files (
                    source_file_id, thread_id, current_path, source_area,
                    device_id, inode, file_generation, observed_size,
                    observed_mtime_ns, file_status, last_seen_at_ms
                 ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                [fixture_path("rollout.jsonl")],
            )
            .unwrap();
    }

    #[test]
    fn spec04_first_binding_reconciles_build_in_same_metadata_transaction() {
        let (db, home) = temp_paths("usage-binding-build");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO threads (
                        thread_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms
                     ) VALUES ('thread',NULL,'thread','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1 WHERE id=1",
                    [crate::usage::USAGE_PARSER_VERSION],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1], 1)
                .unwrap();
        }
        {
            let connection = ledger.connection().unwrap();
            let frozen: (Option<String>, Option<String>) = connection
                .query_row(
                    "SELECT expected_owning_thread_id,expected_root_session_id
                     FROM usage_build_sources WHERE build_epoch=2 AND source_file_id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(frozen, (None, None));
            // Inject a SQLite failure at the usage-build replacement boundary.
            // This is test-only fault injection: the metadata writes happen first
            // in the same transaction, so the trigger proves they roll back if
            // usage reconciliation cannot finish.
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_spec04_binding_reconcile
                     BEFORE DELETE ON usage_build_sources
                     BEGIN
                       SELECT RAISE(ABORT, 'injected usage reconcile failure');
                     END;",
                )
                .unwrap();
        }

        let group = MetadataThreadCommit::new(
            "thread",
            None,
            vec![source_commit_for(1, 1, 10, "thread", None)],
        )
        .unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![group.clone()]).unwrap())
                .is_err()
        );
        {
            let connection = ledger.connection().unwrap();
            let rolled_back: (Option<String>, i64, Option<i64>) = connection
                .query_row(
                    "SELECT
                        (SELECT thread_id FROM source_files WHERE source_file_id=1),
                        (SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id=1),
                        (SELECT committed_offset FROM source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='metadata')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(rolled_back, (None, 0, None));
            connection
                .execute_batch("DROP TRIGGER fail_spec04_binding_reconcile;")
                .unwrap();
        }

        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        let connection = ledger.connection().unwrap();
        let committed: (
            Option<String>,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
        ) = connection
            .query_row(
                "SELECT
                        (SELECT thread_id FROM source_files WHERE source_file_id=1),
                        (SELECT committed_offset FROM source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='metadata'),
                        (SELECT processing_status FROM source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='usage'),
                        (SELECT expected_owning_thread_id FROM usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1),
                        (SELECT expected_root_session_id FROM usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1),
                        (SELECT completion_status FROM usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            committed,
            (
                Some("thread".to_owned()),
                10,
                "rebuild_required".to_owned(),
                Some("thread".to_owned()),
                Some("thread".to_owned()),
                "pending".to_owned(),
            )
        );
        let _ = fs::remove_file(db);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn commits_first_binding_fact_checkpoint_and_patch_atomically() {
        let (db, home) = temp_paths("commit");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        assert_eq!(
            ledger.source_binding_status().unwrap(),
            SourceBindingStatus::Ready
        );
        insert_source(&ledger);

        let mut patch = ResolvedThreadPatch::new("thread", 10).unwrap();
        patch.agent_role = Patch::Set(AgentRole::Main);
        patch.title = Patch::Set("A title".to_owned());
        let group =
            MetadataThreadCommit::new("thread", Some(patch), vec![source_commit(None)]).unwrap();
        let outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(outcome.committed_group_count, 1);
        assert_eq!(outcome.data_revision, 2);
        assert!(outcome.data_changed);

        let connection = ledger.connection().unwrap();
        let binding: Option<String> = connection
            .query_row(
                "SELECT thread_id FROM source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding.as_deref(), Some("thread"));
        let fact_count: i64 = connection
            .query_row("SELECT count(*) FROM rollout_metadata_facts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fact_count, 1);
        let checkpoint_count: i64 = connection
            .query_row("SELECT count(*) FROM source_checkpoints WHERE source_file_id = 1 AND consumer_kind = 'metadata'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(checkpoint_count, 1);
        let title: String = connection
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "A title");
    }

    #[test]
    fn metadata_fact_ordering_and_relationship_conflict_round_trip() {
        let (db, home) = temp_paths("fact-ordering");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);

        let mut fact = source_fact_for(1, 1, 10, "thread");
        fact.latest_context_model = Some("gpt-5.6-sol".to_owned());
        fact.latest_context_turn_id = Some("00000000-0000-7000-8000-000000000001".to_owned());
        fact.latest_context_at_ms = Some(42);
        fact.fact_quality_status = FactQualityStatus::Conflict;
        fact.relationship_conflict = true;
        let source = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            fact,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![source]).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();

        let state = ledger.load_metadata_scan_state([1]).unwrap();
        let SafeFactState::Matching(fact) = &state.entries[0].safe_fact else {
            panic!("metadata fact should match after round trip");
        };
        assert_eq!(
            fact.latest_context_turn_id.as_deref(),
            Some("00000000-0000-7000-8000-000000000001")
        );
        assert_eq!(fact.latest_context_model.as_deref(), Some("gpt-5.6-sol"));
        assert!(fact.relationship_conflict);
        assert_eq!(fact.fact_quality_status, FactQualityStatus::Conflict);
    }

    #[test]
    fn source_only_repeat_does_not_advance_data_revision() {
        let (db, home) = temp_paths("source-only");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);
        let first = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        let first_outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![first]).unwrap())
            .unwrap();
        assert_eq!(first_outcome.data_revision, 1);

        let second = MetadataThreadCommit::new(
            "thread",
            None,
            vec![source_commit(Some("thread".to_owned()))],
        )
        .unwrap();
        let second_outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![second]).unwrap())
            .unwrap();
        assert_eq!(second_outcome.data_revision, 1);
        assert!(!second_outcome.data_changed);
    }

    #[test]
    fn stale_patch_rolls_back_source_and_checkpoint() {
        let (db, home) = temp_paths("rollback");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);
        let initial = MetadataThreadCommit::new(
            "thread",
            Some({
                let mut patch = ResolvedThreadPatch::new("thread", 20).unwrap();
                patch.agent_role = Patch::Set(AgentRole::Main);
                patch
            }),
            vec![source_commit(None)],
        )
        .unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![initial]).unwrap())
            .unwrap();

        let mut stale_patch = ResolvedThreadPatch::new("thread", 19).unwrap();
        stale_patch.title = Patch::Set("must not write".to_owned());
        let stale = MetadataThreadCommit::new(
            "thread",
            Some(stale_patch),
            vec![source_commit(Some("thread".to_owned()))],
        )
        .unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![stale]).unwrap())
                .is_err()
        );

        let connection = ledger.connection().unwrap();
        let title: Option<String> = connection
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, None);
        let offset: i64 = connection
            .query_row("SELECT committed_offset FROM source_checkpoints WHERE source_file_id = 1 AND consumer_kind = 'metadata'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(offset, 10);
    }

    #[test]
    fn patch_only_creates_and_reapplies_a_thread_without_sources() {
        let (db, home) = temp_paths("patch-only");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        let mut patch = ResolvedThreadPatch::new("thread", 10).unwrap();
        patch.agent_role = Patch::Set(AgentRole::Main);
        patch.title = Patch::Set("title".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(patch), Vec::new()).unwrap();
        let outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(outcome.data_revision, 2);

        let mut no_change = ResolvedThreadPatch::new("thread", 10).unwrap();
        no_change.agent_role = Patch::Keep;
        let group = MetadataThreadCommit::new("thread", Some(no_change), Vec::new()).unwrap();
        let outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(outcome.data_revision, 2);
        assert!(!outcome.data_changed);
    }

    #[test]
    fn t_s01_002_project_kind_is_stable_metadata_and_preserves_project_facts() {
        let (db, home) = temp_paths("project-kind");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();

        let mut initial = ResolvedThreadPatch::new("thread", 10).unwrap();
        initial.agent_role = Patch::Set(AgentRole::Main);
        initial.project_name = Patch::Set("Existing name".to_owned());
        initial.project_path = Patch::Set(fixture_path("existing-project"));
        initial.project_kind = Patch::Set(ProjectKind::Project);
        let group = MetadataThreadCommit::new("thread", Some(initial), Vec::new()).unwrap();
        let first = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(first.data_revision, 2);

        let mut projectless = ResolvedThreadPatch::new("thread", 11).unwrap();
        projectless.project_kind = Patch::Set(ProjectKind::Projectless);
        let group = MetadataThreadCommit::new("thread", Some(projectless), Vec::new()).unwrap();
        let second = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(second.data_revision, 3);

        let mut unchanged = ResolvedThreadPatch::new("thread", 11).unwrap();
        unchanged.project_kind = Patch::Set(ProjectKind::Projectless);
        let group = MetadataThreadCommit::new("thread", Some(unchanged), Vec::new()).unwrap();
        let third = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(third.data_revision, 3);
        assert!(!third.data_changed);

        let row: (String, String, String) = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT project_kind,project_path,project_name FROM threads WHERE thread_id='thread'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "projectless".to_owned(),
                fixture_path("existing-project"),
                "Existing name".to_owned()
            )
        );
        let projection = ledger.load_existing_threads().unwrap();
        assert_eq!(projection[0].project_kind, ProjectKind::Projectless);
        let _ = fs::remove_file(db);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn keep_set_and_full_resolution_clear_have_expected_effects() {
        let (db, home) = temp_paths("tri-state");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        let mut initial = ResolvedThreadPatch::new("thread", 10).unwrap();
        initial.agent_role = Patch::Set(AgentRole::Main);
        initial.title = Patch::Set("old".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(initial), Vec::new()).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();

        let mut keep = ResolvedThreadPatch::new("thread", 11).unwrap();
        keep.title = Patch::Keep;
        let group = MetadataThreadCommit::new("thread", Some(keep), Vec::new()).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        let title: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("old"));

        let mut set = ResolvedThreadPatch::new("thread", 12).unwrap();
        set.title = Patch::Set("new".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(set), Vec::new()).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();

        let mut clear = ResolvedThreadPatch::new("thread", 13)
            .unwrap()
            .full_resolution(true);
        clear.title = Patch::Clear;
        let group = MetadataThreadCommit::new("thread", Some(clear), Vec::new()).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        let title: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, None);

        let mut illegal_clear = ResolvedThreadPatch::new("thread", 14).unwrap();
        illegal_clear.title = Patch::Clear;
        let group = MetadataThreadCommit::new("thread", Some(illegal_clear), Vec::new());
        assert!(group.is_err());
    }

    #[test]
    fn usage_checkpoint_is_not_modified_by_metadata_commit() {
        let (db, home) = temp_paths("usage-preserved");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);
        ledger
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version, committed_offset,
                    guard_hash, processing_status
                 ) VALUES (1, 'usage', 7, 5, x'AA', 'ready')",
                [],
            )
            .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        let checkpoint: (i64, i64, Vec<u8>) = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT parser_version, committed_offset, guard_hash
                 FROM source_checkpoints
                 WHERE source_file_id = 1 AND consumer_kind = 'usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(checkpoint, (7, 5, vec![0xAA]));
    }

    #[test]
    fn cas_offset_provenance_and_continuation_fail_before_writes() {
        let (db, home) = temp_paths("preconditions");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);

        // Fact/checkpoint offsets must be equal even though the domain command
        // can be assembled before storage sees the source row.
        let mut mismatched_fact = source_fact_for(1, 1, 9, "thread");
        mismatched_fact.updated_at_ms = 9;
        let mismatch = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            mismatched_fact,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![mismatch]).unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
                .is_err()
        );

        let mut unstable = source_fact_for(1, 1, 10, "thread");
        unstable.continuation_state = ContinuationState::Unstable;
        unstable.ownership_confidence = crate::domain::OwnershipConfidence::Unresolved;
        let unstable = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            unstable,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![unstable]).unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
                .is_err()
        );

        // First bind successfully, then stale expected_previous_thread_id is
        // rejected and cannot advance metadata a second time.
        let first = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        ledger
            .commit_metadata(MetadataCommitBatch::new(vec![first]).unwrap())
            .unwrap();
        let stale = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![stale]).unwrap())
                .is_err()
        );
        let revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1);
    }

    #[test]
    fn generation_id_and_provenance_conflicts_are_rejected() {
        let (db, home) = temp_paths("identity-conflicts");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);

        let generation_mismatch = source_commit_for(1, 2, 10, "thread", None);
        let group = MetadataThreadCommit::new("thread", None, vec![generation_mismatch]).unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
                .is_err()
        );

        let mut wrong_owner = source_fact_for(1, 1, 10, "other");
        wrong_owner.ownership_confidence = crate::domain::OwnershipConfidence::Unresolved;
        let wrong_owner = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            wrong_owner,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        );
        assert!(wrong_owner.is_err());

        let mut bad_provenance = source_fact_for(1, 1, 10, "thread");
        bad_provenance.cwd = Some(fixture_path("tmp"));
        bad_provenance.cwd_provenance = Some(CwdProvenance::SessionMeta);
        bad_provenance.cwd_record_offset = None;
        let bad_provenance = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            bad_provenance,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        );
        assert!(bad_provenance.is_err());

        let mut bad_patch = ResolvedThreadPatch::new("other", 1).unwrap();
        bad_patch.title = Patch::Set("mismatch".to_owned());
        assert!(MetadataThreadCommit::new("thread", Some(bad_patch), Vec::new()).is_err());
    }

    #[test]
    fn source_changed_codex_home_blocks_metadata_writes() {
        let (db, home_a) = temp_paths("source-ready");
        let ledger_a = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();
        insert_source(&ledger_a);
        let home_b = home_a.with_file_name("codex-b");
        fs::create_dir_all(&home_b).unwrap();
        let _ledger_b = Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        assert_eq!(
            ledger_a
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
                .unwrap_err()
                .kind(),
            crate::storage::StorageErrorKind::SourceChanged
        );
        let binding: Option<String> = ledger_a
            .connection()
            .unwrap()
            .query_row(
                "SELECT thread_id FROM source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding, None);
    }

    #[test]
    fn t_s05_014_015_revision_watch_publishes_only_postcommit_and_coalesces_latest_tuple() {
        let (db, home) = temp_paths("spec05-revision-watch");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        insert_source(&ledger);

        let mut receiver = ledger.subscribe_revisions();
        assert!(!receiver.has_changed().unwrap());

        // A status-only commit is published after its SQLite transaction commits.
        ledger
            .mark_scan_started(
                ScanStartEvent::new(
                    "00000000-0000-4000-8000-000000000501",
                    ScanTrigger::Manual,
                    10,
                )
                .unwrap(),
            )
            .unwrap();
        // Do not consume the watch notification yet.  The following data-only
        // metadata commit must coalesce with it into the latest revision tuple.
        let mut patch = ResolvedThreadPatch::new("thread", 10).unwrap();
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group =
            MetadataThreadCommit::new("thread", Some(patch), vec![source_commit(None)]).unwrap();
        let outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(outcome.data_revision, 2);

        assert!(receiver.has_changed().unwrap());
        let latest = *receiver.borrow_and_update();
        assert_eq!(latest.data_revision, 2);
        assert_eq!(latest.status_revision, 1);
        assert!(!receiver.has_changed().unwrap());

        // A failing metadata transaction must not publish a revision that was
        // never committed.  Reusing the stale expected previous binding makes
        // the production CAS fail before any durable write.
        let mut stale_patch = ResolvedThreadPatch::new("thread", 20).unwrap();
        stale_patch.title = Patch::Set("should-not-commit".to_owned());
        let stale_group =
            MetadataThreadCommit::new("thread", Some(stale_patch), vec![source_commit(None)])
                .unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![stale_group]).unwrap())
                .is_err()
        );
        assert_eq!(ledger.current_revision(), latest);
        assert!(!receiver.has_changed().unwrap());
    }

    #[test]
    fn multiple_sources_commit_as_one_group_and_rollback_together() {
        let (db, home) = temp_paths("multi-source");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-a.jsonl")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (2, NULL, ?1, 'archived_sessions', 1, 3, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-b.jsonl")],
                )
                .unwrap();
        }
        let mut patch = ResolvedThreadPatch::new("thread", 10).unwrap();
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group = MetadataThreadCommit::new(
            "thread",
            Some(patch),
            vec![
                source_commit_for(1, 1, 10, "thread", None),
                source_commit_for(2, 1, 10, "thread", None),
            ],
        )
        .unwrap();
        let outcome = ledger
            .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            .unwrap();
        assert_eq!(outcome.data_revision, 2);
        let binding_count: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM source_files WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding_count, 2);

        let (db, home) = temp_paths("multi-source-rollback");
        let ledger = Ledger::open(LedgerOptions::new(&db, &home)).unwrap();
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-a.jsonl")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (2, 'other', ?1, 'sessions', 1, 3, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-b.jsonl")],
                )
                .unwrap();
        }
        let mut patch = ResolvedThreadPatch::new("thread", 10).unwrap();
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group = MetadataThreadCommit::new(
            "thread",
            Some(patch),
            vec![
                source_commit_for(1, 1, 10, "thread", None),
                source_commit_for(2, 1, 10, "thread", None),
            ],
        )
        .unwrap();
        assert!(
            ledger
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
                .is_err()
        );
        let binding: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT thread_id FROM source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding, None);
        let revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1);
        let fact_count: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM rollout_metadata_facts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fact_count, 0);
    }
}
