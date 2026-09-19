//! Physical rollout observations and independent consumer checkpoints.
//!
//! This module owns the Spec 01 physical-source observation transaction.
//! Spec 04 extends that same transaction with usage-build manifest transitions
//! so no cross-table crash window can exist. Rollout contents are still never
//! parsed here.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::domain::{
    AgentPathProvenance, AgentRoleProvenance, CheckpointOutcome, CheckpointProcessingStatus,
    CheckpointRebuildCommand, ConsumerKind, ContinuationState, CwdProvenance, FactQualityStatus,
    FileStatus, MetadataCheckpointState, MetadataScanState, MetadataScanStateEntry,
    OwnershipConfidence, ParentHintProvenance, RolloutMetadataFact, SafeFactMismatchReason,
    SafeFactState, SourceArea, SourceFileState, SourceObservationBatch, SourceObservationResult,
    SourceOutcome,
};

use super::{Ledger, Result, StorageError};

/// A database source row copied into memory before an observation pass.  The
/// copy lets a batch match paths and physical identities consistently before
/// any UNIQUE(current_path) updates are applied.
#[derive(Clone, Debug)]
struct ExistingSource {
    source_file_id: i64,
    thread_id: Option<String>,
    current_path: String,
    source_area: SourceArea,
    device_id: i64,
    inode: i64,
    file_generation: i64,
    observed_size: i64,
    observed_mtime_ns: i64,
    file_status: FileStatus,
}

#[derive(Clone, Copy, Debug)]
struct ObservationPlan {
    source_file_id: i64,
    file_generation: i64,
    created: bool,
    replaced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageCarryObservationRequirement {
    pub device_id: i64,
    pub inode: i64,
    pub active_committed_offset: i64,
    pub active_guard_hash: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageCarryObservationProof {
    pub device_id: i64,
    pub inode: i64,
    pub active_committed_offset: i64,
    pub guard_matches: bool,
}

impl Ledger {
    /// Record one enumerated set of present rollout files.
    ///
    /// Physical identity (`device_id`, `inode`) is matched before path.  A
    /// path move therefore retains its MU source id and generation.  A
    /// replacement, truncation, or same-size rewrite advances generation and
    /// invalidates every existing consumer checkpoint and safe metadata fact
    /// in the same transaction.  Spec 01 has no complete-region signal, so
    /// observations that are absent from a region marked `Unavailable` are
    /// intentionally untouched.  Only a region marked `Complete` proves that
    /// absence means a source became missing.
    pub fn record_source_observations(
        &self,
        batch: SourceObservationBatch,
    ) -> Result<SourceOutcome> {
        self.record_source_observations_with_usage_carry_proofs(batch, &[])
    }

    pub(crate) fn load_usage_carry_observation_requirements(
        &self,
    ) -> Result<Vec<UsageCarryObservationRequirement>> {
        let connection = self.connection()?;
        let build_epoch: Option<i64> = connection.query_row(
            "SELECT usage_build_epoch FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        let Some(build_epoch) = build_epoch else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            "SELECT b.expected_device_id,b.expected_inode,b.active_committed_offset,b.active_guard_hash
             FROM usage_build_sources b
             WHERE b.build_epoch=?1 AND b.carry_phase<>'none'
             ORDER BY b.source_file_id",
        )?;
        let rows = statement.query_map([build_epoch], |row| {
            Ok(UsageCarryObservationRequirement {
                device_id: row.get(0)?,
                inode: row.get(1)?,
                active_committed_offset: row.get(2)?,
                active_guard_hash: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn record_source_observations_with_usage_carry_proofs(
        &self,
        batch: SourceObservationBatch,
        usage_carry_proofs: &[UsageCarryObservationProof],
    ) -> Result<SourceOutcome> {
        batch
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        self.ensure_source_ready()?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_source_binding(&transaction, self.expected_codex_home_fingerprint())?;

        let existing = load_existing_sources(&transaction)?;
        let plans = plan_observations(&existing, &batch)?;

        // Free paths that are going to be moved before assigning their final
        // paths.  This also handles a two-file path swap in one observation
        // batch without violating UNIQUE(current_path).
        let mut temporary_paths = Vec::new();
        for (index, plan) in plans.iter().enumerate() {
            if plan.created {
                continue;
            }
            let old = existing
                .iter()
                .find(|source| source.source_file_id == plan.source_file_id)
                .ok_or_else(|| invalid_state("observation plan references an unknown source"))?;
            let observation = &batch.observations[index];
            if old.current_path != observation.current_path {
                let temporary = temporary_path(plan.source_file_id, old.file_generation);
                transaction.execute(
                    "UPDATE source_files SET current_path = ?2 WHERE source_file_id = ?1",
                    params![plan.source_file_id, temporary],
                )?;
                temporary_paths.push((plan.source_file_id, temporary));
            }
        }

        let mut results = Vec::with_capacity(plans.len());
        for (index, plan) in plans.iter().enumerate() {
            let observation = &batch.observations[index];
            let (thread_id, old_path, old_area) = if plan.created {
                (None, None, None)
            } else {
                let old = existing
                    .iter()
                    .find(|source| source.source_file_id == plan.source_file_id)
                    .ok_or_else(|| {
                        invalid_state("observation plan references an unknown source")
                    })?;
                (
                    old.thread_id.clone(),
                    Some(old.current_path.clone()),
                    Some(old.source_area),
                )
            };

            if plan.created {
                transaction.execute(
                    "INSERT INTO source_files (
                        thread_id, current_path, source_area, device_id, inode,
                        file_generation, observed_size, observed_mtime_ns,
                        file_status, last_seen_at_ms
                    ) VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'present', ?8)",
                    params![
                        observation.current_path,
                        observation.source_area.as_str(),
                        observation.device_id,
                        observation.inode,
                        plan.file_generation,
                        observation.observed_size,
                        observation.observed_mtime_ns,
                        observation.last_seen_at_ms,
                    ],
                )?;
                let source_file_id = transaction.last_insert_rowid();
                transaction.execute(
                    "INSERT INTO source_checkpoints (
                        source_file_id, consumer_kind, parser_version,
                        committed_offset, guard_hash, processing_status,
                        last_successful_scan_at_ms, last_error_code
                    ) VALUES (?1, 'metadata', 0, 0, NULL, 'pending', NULL, NULL)",
                    [source_file_id],
                )?;
                results.push(SourceObservationResult {
                    source_file_id,
                    file_generation: plan.file_generation,
                    created: true,
                    moved: false,
                    replaced: false,
                    rebuild_consumers: Vec::new(),
                    build_disposition: crate::domain::BuildDisposition::Unchanged,
                });
                continue;
            }

            let moved = old_path.as_deref() != Some(observation.current_path.as_str())
                || old_area != Some(observation.source_area);
            let replaced = plan.replaced;

            transaction.execute(
                "UPDATE source_files SET
                    thread_id = ?2,
                    current_path = ?3,
                    source_area = ?4,
                    device_id = ?5,
                    inode = ?6,
                    file_generation = ?7,
                    observed_size = ?8,
                    observed_mtime_ns = ?9,
                    file_status = 'present',
                    last_seen_at_ms = ?10
                 WHERE source_file_id = ?1",
                params![
                    plan.source_file_id,
                    if replaced {
                        None::<String>
                    } else {
                        thread_id.clone()
                    },
                    observation.current_path,
                    observation.source_area.as_str(),
                    observation.device_id,
                    observation.inode,
                    plan.file_generation,
                    observation.observed_size,
                    observation.observed_mtime_ns,
                    observation.last_seen_at_ms,
                ],
            )?;

            let mut rebuild_consumers = Vec::new();
            if replaced {
                transaction.execute(
                    "DELETE FROM rollout_metadata_facts WHERE source_file_id = ?1",
                    [plan.source_file_id],
                )?;
                let mut checkpoint_rows = transaction.prepare(
                    "SELECT consumer_kind FROM source_checkpoints
                     WHERE source_file_id = ?1 ORDER BY consumer_kind",
                )?;
                let consumers = checkpoint_rows
                    .query_map([plan.source_file_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(checkpoint_rows);
                for consumer in consumers {
                    let consumer =
                        ConsumerKind::try_from(consumer.as_str()).map_err(domain_sql_error)?;
                    rebuild_consumers.push(consumer);
                }
                transaction.execute(
                    "UPDATE source_checkpoints SET
                        committed_offset = 0,
                        guard_hash = NULL,
                        processing_status = 'rebuild_required',
                        last_successful_scan_at_ms = NULL,
                        last_error_code = NULL
                     WHERE source_file_id = ?1",
                    [plan.source_file_id],
                )?;
            }

            // Every rollout must have a metadata checkpoint.  A source that
            // predates this module (or was manually repaired) gets the same
            // offset-zero pending checkpoint as a newly discovered source.
            transaction.execute(
                "INSERT OR IGNORE INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status,
                    last_successful_scan_at_ms, last_error_code
                ) VALUES (?1, 'metadata', 0, 0, NULL, 'pending', NULL, NULL)",
                [plan.source_file_id],
            )?;

            results.push(SourceObservationResult {
                source_file_id: plan.source_file_id,
                file_generation: plan.file_generation,
                created: false,
                moved,
                replaced,
                rebuild_consumers,
                build_disposition: crate::domain::BuildDisposition::Unchanged,
            });
        }

        // Mark only sources in completely observed regions that were absent
        // from this batch.  An unavailable region provides no evidence about
        // files that were not returned by the scanner.
        let observed_source_ids = results
            .iter()
            .map(|result| result.source_file_id)
            .collect::<HashSet<_>>();
        for (area, status) in [
            (SourceArea::Sessions, &batch.sessions),
            (SourceArea::ArchivedSessions, &batch.archived_sessions),
        ] {
            if !status.is_complete() {
                continue;
            }
            for source in existing
                .iter()
                .filter(|source| source.source_area == area)
                .filter(|source| source.file_status == FileStatus::Present)
                .filter(|source| !observed_source_ids.contains(&source.source_file_id))
            {
                transaction.execute(
                    "UPDATE source_files SET file_status = 'missing'
                     WHERE source_file_id = ?1 AND file_status = 'present'",
                    [source.source_file_id],
                )?;
            }
        }

        let observed_ids = results
            .iter()
            .map(|result| result.source_file_id)
            .collect::<Vec<_>>();
        let usage_carry_proofs = usage_carry_proofs
            .iter()
            .map(|proof| ((proof.device_id, proof.inode), proof))
            .collect::<HashMap<_, _>>();
        crate::usage::rebuild::apply_source_observations_to_build_tx(
            &transaction,
            &observed_ids,
            &mut results,
            &usage_carry_proofs,
            batch
                .observations
                .iter()
                .map(|observation| observation.last_seen_at_ms)
                .max()
                .unwrap_or(0),
        )
        .map_err(|error| StorageError::invalid_state(error.to_string()))?;

        // `temporary_paths` exists only to make the operation's intent clear
        // in diagnostics and to keep the compiler from treating the first
        // phase as an accidental no-op.  All paths have been restored by the
        // second phase; no temporary value is ever committed.
        let _ = temporary_paths;
        transaction.commit()?;

        SourceOutcome::new(results).map_err(|error| StorageError::invalid_state(error.to_string()))
    }

    /// Read source rows, metadata checkpoints and safe facts from one SQLite
    /// snapshot.  The generic input accepts either a slice or an owned vector,
    /// while preserving the caller's requested order in the returned entries.
    pub fn load_metadata_scan_state<I>(&self, source_file_ids: I) -> Result<MetadataScanState>
    where
        I: AsRef<[i64]>,
    {
        let ids = source_file_ids.as_ref();
        validate_ids(ids)?;
        if ids.is_empty() {
            return Ok(MetadataScanState {
                entries: Vec::new(),
            });
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut entries = Vec::with_capacity(ids.len());
        let mut seen = HashSet::with_capacity(ids.len());
        for source_file_id in ids {
            if !seen.insert(*source_file_id) {
                return Err(invalid_state(
                    "duplicate source_file_id in metadata scan state",
                ));
            }
            let source = query_source_state(&transaction, *source_file_id)?.ok_or_else(|| {
                invalid_state(format!("source file {source_file_id} does not exist"))
            })?;
            let checkpoint = query_metadata_checkpoint(&transaction, *source_file_id)?;
            let fact = query_metadata_fact(&transaction, *source_file_id)?;
            let safe_fact = classify_safe_fact(&source, checkpoint.as_ref(), fact);
            entries.push(MetadataScanStateEntry {
                source,
                metadata_checkpoint: checkpoint,
                safe_fact,
            });
        }
        transaction.commit()?;
        MetadataScanState::new(entries)
            .map_err(|error| StorageError::invalid_state(error.to_string()))
    }

    /// Mark one consumer's checkpoints as requiring a complete rebuild.
    ///
    /// Existing rows are reset to offset zero.  The selected consumer is the
    /// only row touched: a metadata rebuild never changes usage progress and a
    /// usage rebuild never creates a usage checkpoint in the Spec 01 schema.
    pub fn require_checkpoint_rebuild(
        &self,
        command: CheckpointRebuildCommand,
    ) -> Result<CheckpointOutcome> {
        command
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        self.ensure_source_ready()?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_source_binding(&transaction, self.expected_codex_home_fingerprint())?;

        for source_file_id in &command.source_file_ids {
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT source_file_id FROM source_files WHERE source_file_id = ?1",
                    [source_file_id],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(invalid_state(format!(
                    "source file {source_file_id} does not exist"
                )));
            }
            let updated = transaction.execute(
                "UPDATE source_checkpoints SET
                    committed_offset = 0,
                    guard_hash = NULL,
                    processing_status = 'rebuild_required',
                    last_successful_scan_at_ms = NULL,
                    last_error_code = NULL
                 WHERE source_file_id = ?1 AND consumer_kind = ?2",
                params![source_file_id, command.consumer_kind.as_str()],
            )?;
            if updated != 1 {
                return Err(invalid_state(format!(
                    "{} checkpoint for source file {source_file_id} does not exist",
                    command.consumer_kind.as_str()
                )));
            }
        }
        transaction.commit()?;

        Ok(CheckpointOutcome {
            consumer_kind: command.consumer_kind,
            source_file_ids: command.source_file_ids,
        })
    }
}

fn plan_observations(
    existing: &[ExistingSource],
    batch: &SourceObservationBatch,
) -> Result<Vec<ObservationPlan>> {
    let by_path = existing
        .iter()
        .map(|source| (source.current_path.as_str(), source))
        .collect::<HashMap<_, _>>();
    let mut by_identity: HashMap<(i64, i64), &ExistingSource> = HashMap::new();
    for source in existing {
        by_identity
            .entry((source.device_id, source.inode))
            .and_modify(|current| {
                if source.file_generation > current.file_generation {
                    *current = source;
                }
            })
            .or_insert(source);
    }

    // Assign the complete batch by physical identity first.  Doing this for
    // every observation before consulting paths is what makes path swaps and
    // "move away while a new file occupies the old path" deterministic.
    let mut assignments = vec![None; batch.observations.len()];
    let mut used_sources = HashSet::with_capacity(batch.observations.len());
    for (index, observation) in batch.observations.iter().enumerate() {
        if let Some(source) = by_identity
            .get(&(observation.device_id, observation.inode))
            .copied()
        {
            if !used_sources.insert(source.source_file_id) {
                return Err(invalid_state(
                    "one source file matched more than once by physical identity",
                ));
            }
            assignments[index] = Some(source);
        }
    }

    // Only observations without a physical match may claim an unassigned path
    // slot as a replacement.  If the slot owner was already claimed by its
    // physical identity, it is moving elsewhere and this observation is a new
    // source occupying the vacated path.
    for (index, observation) in batch.observations.iter().enumerate() {
        if assignments[index].is_some() {
            continue;
        }
        let Some(source) = by_path.get(observation.current_path.as_str()).copied() else {
            continue;
        };
        if used_sources.insert(source.source_file_id) {
            assignments[index] = Some(source);
        }
    }

    // A physical move can target a path only when that path is free or its
    // current owner is also represented in this batch and will move away.
    // Without that evidence, changing either row would guess at missing state.
    for (index, source) in assignments.iter().enumerate() {
        let Some(source) = source else {
            continue;
        };
        let observation = &batch.observations[index];
        if source.current_path == observation.current_path {
            continue;
        }
        if let Some(occupant) = by_path.get(observation.current_path.as_str()).copied()
            && occupant.source_file_id != source.source_file_id
            && !used_sources.contains(&occupant.source_file_id)
        {
            return Err(invalid_state(format!(
                "target path {} is occupied by an unobserved source",
                observation.current_path
            )));
        }
    }

    let mut plans = Vec::with_capacity(batch.observations.len());
    for (index, assignment) in assignments.into_iter().enumerate() {
        let observation = &batch.observations[index];
        let Some(source) = assignment else {
            plans.push(ObservationPlan {
                source_file_id: 0,
                file_generation: 1,
                created: true,
                replaced: false,
            });
            continue;
        };
        let identity_same =
            source.device_id == observation.device_id && source.inode == observation.inode;
        let generation_changed = !identity_same
            || (source.file_status != FileStatus::Missing
                && (observation.observed_size < source.observed_size
                    || (observation.observed_size == source.observed_size
                        && observation.observed_mtime_ns != source.observed_mtime_ns)));
        let file_generation = if generation_changed {
            source
                .file_generation
                .checked_add(1)
                .ok_or_else(|| invalid_state("file_generation overflow"))?
        } else {
            source.file_generation
        };
        plans.push(ObservationPlan {
            source_file_id: source.source_file_id,
            file_generation,
            created: false,
            replaced: generation_changed,
        });
    }
    Ok(plans)
}

fn temporary_path(source_file_id: i64, generation: i64) -> String {
    format!("/.usagi-observation-pending/{source_file_id}-{generation}")
}

fn load_existing_sources(transaction: &Transaction<'_>) -> Result<Vec<ExistingSource>> {
    let mut statement = transaction.prepare(
        "SELECT source_file_id, thread_id, current_path, source_area,
                device_id, inode, file_generation, observed_size,
                observed_mtime_ns, file_status
         FROM source_files",
    )?;
    let rows = statement.query_map([], |row| {
        let source_area: String = row.get(3)?;
        let file_status: String = row.get(9)?;
        Ok(ExistingSource {
            source_file_id: row.get(0)?,
            thread_id: row.get(1)?,
            current_path: row.get(2)?,
            source_area: SourceArea::try_from(source_area.as_str()).map_err(domain_sql_error)?,
            device_id: row.get(4)?,
            inode: row.get(5)?,
            file_generation: row.get(6)?,
            observed_size: row.get(7)?,
            observed_mtime_ns: row.get(8)?,
            file_status: FileStatus::try_from(file_status.as_str()).map_err(domain_sql_error)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_source_state(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> Result<Option<SourceFileState>> {
    transaction
        .query_row(
            "SELECT source_file_id, thread_id, current_path, source_area,
                    device_id, inode, file_generation, observed_size,
                    observed_mtime_ns, file_status, last_seen_at_ms
             FROM source_files WHERE source_file_id = ?1",
            [source_file_id],
            |row| {
                let source_area: String = row.get(3)?;
                let file_status: String = row.get(9)?;
                SourceFileState::new(
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                    SourceArea::try_from(source_area.as_str()).map_err(domain_sql_error)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    FileStatus::try_from(file_status.as_str()).map_err(domain_sql_error)?,
                    row.get(10)?,
                )
                .map_err(domain_sql_error)
            },
        )
        .optional()
        .map_err(Into::into)
}

fn query_metadata_checkpoint(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> Result<Option<MetadataCheckpointState>> {
    transaction
        .query_row(
            "SELECT parser_version, committed_offset, guard_hash,
                    processing_status, last_successful_scan_at_ms, last_error_code
             FROM source_checkpoints
             WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
            [source_file_id],
            |row| {
                let processing_status: String = row.get(3)?;
                MetadataCheckpointState::new(
                    source_file_id,
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    CheckpointProcessingStatus::try_from(processing_status.as_str())
                        .map_err(domain_sql_error)?,
                    row.get(4)?,
                    row.get(5)?,
                )
                .map_err(domain_sql_error)
            },
        )
        .optional()
        .map_err(Into::into)
}

fn query_metadata_fact(
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> Result<Option<RolloutMetadataFact>> {
    transaction
        .query_row(
            "SELECT source_file_id, file_generation, metadata_parser_version,
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
                let continuation_state: String = row.get(5)?;
                let cwd_provenance: Option<String> = row.get(7)?;
                let parent_hint_provenance: Option<String> = row.get(13)?;
                let agent_role_provenance: Option<String> = row.get(16)?;
                let agent_path_provenance: Option<String> = row.get(19)?;
                let ownership_confidence: String = row.get(23)?;
                let fact_quality_status: String = row.get(24)?;
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
                let value = RolloutMetadataFact {
                    source_file_id: row.get(0)?,
                    file_generation: row.get(1)?,
                    metadata_parser_version: row.get(2)?,
                    resolved_through_offset: row.get(3)?,
                    owning_thread_id: row.get(4)?,
                    continuation_state: ContinuationState::try_from(continuation_state.as_str())
                        .map_err(domain_sql_error)?,
                    cwd: row.get(6)?,
                    cwd_provenance: cwd_provenance
                        .as_deref()
                        .map(CwdProvenance::try_from)
                        .transpose()
                        .map_err(domain_sql_error)?,
                    cwd_record_offset: row.get(8)?,
                    created_at_ms: row.get(9)?,
                    latest_context_model: row.get(10)?,
                    latest_context_at_ms: row.get(11)?,
                    latest_context_turn_id: row.get(26)?,
                    parent_thread_id_hint: row.get(12)?,
                    parent_hint_provenance: parent_hint_provenance
                        .as_deref()
                        .map(ParentHintProvenance::try_from)
                        .transpose()
                        .map_err(domain_sql_error)?,
                    parent_hint_record_offset: row.get(14)?,
                    agent_role_hint: row.get(15)?,
                    agent_role_provenance: agent_role_provenance
                        .as_deref()
                        .map(AgentRoleProvenance::try_from)
                        .transpose()
                        .map_err(domain_sql_error)?,
                    agent_role_record_offset: row.get(17)?,
                    agent_path: row.get(18)?,
                    agent_path_provenance: agent_path_provenance
                        .as_deref()
                        .map(AgentPathProvenance::try_from)
                        .transpose()
                        .map_err(domain_sql_error)?,
                    agent_path_record_offset: row.get(20)?,
                    replay_start_offset: row.get(21)?,
                    owning_records_start_offset: row.get(22)?,
                    ownership_confidence: OwnershipConfidence::try_from(
                        ownership_confidence.as_str(),
                    )
                    .map_err(domain_sql_error)?,
                    fact_quality_status: FactQualityStatus::try_from(fact_quality_status.as_str())
                        .map_err(domain_sql_error)?,
                    updated_at_ms: row.get(25)?,
                    relationship_conflict,
                };
                Ok(value)
            },
        )
        .optional()
        .map_err(Into::into)
}

fn classify_safe_fact(
    source: &SourceFileState,
    checkpoint: Option<&MetadataCheckpointState>,
    fact: Option<RolloutMetadataFact>,
) -> SafeFactState {
    let Some(fact) = fact else {
        return SafeFactState::None;
    };
    if fact.validate().is_err() {
        return SafeFactState::Stale(SafeFactMismatchReason::InvalidFact);
    }
    if source.file_status != FileStatus::Present {
        return SafeFactState::Stale(SafeFactMismatchReason::SourceMissing);
    }
    let Some(checkpoint) = checkpoint else {
        return SafeFactState::Stale(SafeFactMismatchReason::MissingCheckpoint);
    };
    if checkpoint.processing_status == CheckpointProcessingStatus::RebuildRequired {
        return SafeFactState::Stale(SafeFactMismatchReason::InvalidFact);
    }
    if fact.source_file_id != source.source_file_id {
        return SafeFactState::Stale(SafeFactMismatchReason::InvalidFact);
    }
    if fact.file_generation != source.file_generation {
        return SafeFactState::Stale(SafeFactMismatchReason::GenerationMismatch);
    }
    if fact.metadata_parser_version != checkpoint.parser_version {
        return SafeFactState::Stale(SafeFactMismatchReason::ParserVersionMismatch);
    }
    if fact.resolved_through_offset != checkpoint.committed_offset {
        return SafeFactState::Stale(SafeFactMismatchReason::OffsetMismatch);
    }
    let resolved_through_offset = fact.resolved_through_offset;
    if [
        fact.cwd_record_offset,
        fact.parent_hint_record_offset,
        fact.agent_role_record_offset,
        fact.agent_path_record_offset,
        fact.replay_start_offset,
        fact.owning_records_start_offset,
    ]
    .into_iter()
    .flatten()
    .any(|offset| offset > resolved_through_offset)
    {
        return SafeFactState::Stale(SafeFactMismatchReason::InvalidFact);
    }
    match source.thread_id.as_deref() {
        None => return SafeFactState::Stale(SafeFactMismatchReason::BindingMismatch),
        Some(thread_id) if thread_id != fact.owning_thread_id => {
            return SafeFactState::Stale(SafeFactMismatchReason::OwningThreadMismatch);
        }
        Some(_) => {}
    }
    if fact.continuation_state == ContinuationState::Unstable && fact.resolved_through_offset > 0 {
        return SafeFactState::Stale(SafeFactMismatchReason::ContinuationUnstable);
    }
    match fact.validate_against(source, checkpoint) {
        Ok(()) if fact.ownership_confidence == OwnershipConfidence::Confirmed => {
            SafeFactState::Matching(fact)
        }
        Ok(()) => SafeFactState::Stale(SafeFactMismatchReason::InvalidFact),
        Err(error) => SafeFactState::Stale(mismatch_reason(&error)),
    }
}

fn mismatch_reason(error: &crate::domain::DomainError) -> SafeFactMismatchReason {
    use crate::domain::DomainError;
    match error {
        DomainError::InvariantViolation { invariant } if invariant.contains("generation") => {
            SafeFactMismatchReason::GenerationMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("parser") => {
            SafeFactMismatchReason::ParserVersionMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("offset") => {
            SafeFactMismatchReason::OffsetMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("binding") => {
            SafeFactMismatchReason::BindingMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("owning thread") => {
            SafeFactMismatchReason::OwningThreadMismatch
        }
        _ => SafeFactMismatchReason::InvalidFact,
    }
}

fn verify_source_binding(transaction: &Transaction<'_>, fingerprint: &str) -> Result<()> {
    let (stored_fingerprint, status): (Option<String>, String) = transaction.query_row(
        "SELECT codex_home_fingerprint, source_binding_status FROM app_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    match (stored_fingerprint, status.as_str()) {
        (Some(stored), "ready") if stored == fingerprint => Ok(()),
        (Some(stored), "ready") => Err(StorageError::source_changed(stored, fingerprint)),
        (None, "unbound") => Err(StorageError::source_unbound()),
        (Some(stored), "source_changed") => Err(StorageError::source_changed(stored, fingerprint)),
        (_, other) => Err(invalid_state(format!(
            "invalid source binding status {other:?}"
        ))),
    }
}

fn validate_ids(ids: &[i64]) -> Result<()> {
    for id in ids {
        if *id <= 0 {
            return Err(invalid_state("source_file_id must be positive"));
        }
    }
    Ok(())
}

fn invalid_state(message: impl Into<String>) -> StorageError {
    StorageError::invalid_state(message)
}

fn domain_sql_error(error: crate::domain::DomainError) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;

    use super::*;
    use crate::domain::{
        CheckpointRebuildCommand, ConsumerKind, SourceObservation, SourceRegionStatus,
    };
    use crate::storage::LedgerOptions;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-storage-source")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn test_ledger() -> (Ledger, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "usagi-source-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let root = root.join(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
                .to_string(),
        );
        std::fs::create_dir_all(&root).expect("create temporary test directory");
        let ledger = Ledger::open(LedgerOptions::new(
            root.join("mu.sqlite3"),
            root.join("codex"),
        ))
        .expect("open temporary ledger");
        (ledger, root)
    }

    fn observation(
        path: &str,
        area: SourceArea,
        device_id: i64,
        inode: i64,
        size: i64,
        mtime: i64,
        seen_at: i64,
    ) -> SourceObservation {
        SourceObservation::new(path, area, device_id, inode, size, mtime, seen_at)
            .expect("valid source observation")
    }

    fn one_observation(ledger: &Ledger, value: SourceObservation) -> SourceObservationResult {
        ledger
            .record_source_observations(complete_batch(vec![value]))
            .unwrap()
            .results
            .into_iter()
            .next()
            .unwrap()
    }

    fn complete_batch(observations: Vec<SourceObservation>) -> SourceObservationBatch {
        SourceObservationBatch::new(
            observations,
            SourceRegionStatus::Complete,
            SourceRegionStatus::Complete,
        )
        .unwrap()
    }

    fn cleanup(root: std::path::PathBuf) {
        std::fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[test]
    fn move_keeps_identity_then_replacement_resets_all_consumers() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/rollout.jsonl"),
                SourceArea::Sessions,
                10,
                20,
                100,
                1_000,
                1,
            ),
        );
        assert!(first.created);
        assert_eq!(first.file_generation, 1);
        let source_file_id = first.source_file_id;

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "INSERT INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 4, 88, X'02', 'ready')",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 3, committed_offset = 77,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        let moved = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-archive/rollout.jsonl"),
                SourceArea::ArchivedSessions,
                10,
                20,
                100,
                1_000,
                2,
            ),
        );
        assert_eq!(moved.source_file_id, source_file_id);
        assert_eq!(moved.file_generation, 1);
        assert!(moved.moved);
        assert!(!moved.replaced);

        let connection = Connection::open(ledger.database_path()).unwrap();
        let (path, metadata_offset, usage_offset): (String, i64, i64) = connection
            .query_row(
                "SELECT s.current_path,
                        (SELECT committed_offset FROM source_checkpoints WHERE source_file_id = s.source_file_id AND consumer_kind = 'metadata'),
                        (SELECT committed_offset FROM source_checkpoints WHERE source_file_id = s.source_file_id AND consumer_kind = 'usage')
                 FROM source_files s WHERE source_file_id = ?1",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, fixture_path("mu-archive/rollout.jsonl"));
        assert_eq!(metadata_offset, 77);
        assert_eq!(usage_offset, 88);
        drop(connection);

        let rewritten = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-archive/rollout.jsonl"),
                SourceArea::ArchivedSessions,
                10,
                20,
                100,
                1_001,
                3,
            ),
        );
        assert_eq!(rewritten.source_file_id, source_file_id);
        assert_eq!(rewritten.file_generation, 2);
        assert!(rewritten.replaced);
        assert_eq!(
            rewritten.build_disposition,
            crate::domain::BuildDisposition::Unchanged
        );
        assert_eq!(
            rewritten.rebuild_consumers,
            vec![ConsumerKind::Metadata, ConsumerKind::Usage]
        );

        let connection = Connection::open(ledger.database_path()).unwrap();
        let rows: Vec<(String, i64, Option<Vec<u8>>)> = connection
            .prepare(
                "SELECT consumer_kind, committed_offset, guard_hash
                 FROM source_checkpoints WHERE source_file_id = ?1
                 ORDER BY consumer_kind",
            )
            .unwrap()
            .query_map([source_file_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("metadata".into(), 0, None), ("usage".into(), 0, None)]
        );
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn metadata_fact_matching_and_stale_states_are_explicit() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/fact.jsonl"),
                SourceArea::Sessions,
                3,
                4,
                64,
                2,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_files SET thread_id = 'thread-1' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 7, committed_offset = 64,
                     guard_hash = X'07', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO rollout_metadata_facts (
                    source_file_id, file_generation, metadata_parser_version,
                    resolved_through_offset, owning_thread_id, continuation_state,
                    ownership_confidence, fact_quality_status, updated_at_ms
                 ) VALUES (?1, 1, 7, 64, 'thread-1', 'owning_live', 'confirmed', 'complete', 8)",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
        assert!(matches!(
            state.entries[0].safe_fact,
            SafeFactState::Matching(_)
        ));

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET metadata_parser_version = 6 WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        drop(connection);
        let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
        assert_eq!(
            state.entries[0].safe_fact,
            SafeFactState::Stale(SafeFactMismatchReason::ParserVersionMismatch)
        );

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET metadata_parser_version = 7 WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_files SET file_status = 'missing' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        drop(connection);
        let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
        assert_eq!(
            state.entries[0].safe_fact,
            SafeFactState::Stale(SafeFactMismatchReason::SourceMissing)
        );
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn checkpoint_rebuild_isolated_to_requested_consumer() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/rebuild.jsonl"),
                SourceArea::Sessions,
                5,
                6,
                80,
                1,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 1, committed_offset = 80,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 2, 70, X'02', 'ready')",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        ledger
            .require_checkpoint_rebuild(
                CheckpointRebuildCommand::new(ConsumerKind::Metadata, vec![source_file_id])
                    .unwrap(),
            )
            .unwrap();
        let connection = Connection::open(ledger.database_path()).unwrap();
        let metadata: (i64, Option<Vec<u8>>, String) = connection
            .query_row(
                "SELECT committed_offset, guard_hash, processing_status
                 FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let usage: (i64, Option<Vec<u8>>, String) = connection
            .query_row(
                "SELECT committed_offset, guard_hash, processing_status
                 FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'usage'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(metadata, (0, None, "rebuild_required".into()));
        assert_eq!(usage, (70, Some(vec![2]), "ready".into()));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn missing_source_restores_by_identity_at_new_path_without_reset() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/old.jsonl"),
                SourceArea::Sessions,
                9,
                10,
                20,
                1,
                1,
            ),
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 3, committed_offset = 20,
                     guard_hash = X'03', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [first.source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 4, 19, X'04', 'ready')",
                [first.source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_files SET file_status = 'missing' WHERE source_file_id = ?1",
                [first.source_file_id],
            )
            .unwrap();
        drop(connection);

        let restored = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/new.jsonl"),
                SourceArea::Sessions,
                9,
                10,
                20,
                1,
                2,
            ),
        );
        assert!(!restored.created);
        assert!(restored.moved);
        assert!(!restored.replaced);
        assert_eq!(restored.source_file_id, first.source_file_id);
        assert_eq!(restored.file_generation, 1);
        assert!(restored.rebuild_consumers.is_empty());

        let connection = Connection::open(ledger.database_path()).unwrap();
        let source: (i64, String, String) = connection
            .query_row(
                "SELECT file_generation, current_path, file_status
                 FROM source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            source,
            (1, fixture_path("mu-sessions/new.jsonl"), "present".into())
        );
        let checkpoints: Vec<(String, i64, String)> = connection
            .prepare(
                "SELECT consumer_kind, committed_offset, processing_status
                 FROM source_checkpoints WHERE source_file_id = ?1
                 ORDER BY consumer_kind",
            )
            .unwrap()
            .query_map([first.source_file_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            checkpoints,
            vec![
                ("metadata".into(), 20, "ready".into()),
                ("usage".into(), 19, "ready".into())
            ]
        );
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn two_sources_can_swap_paths_by_physical_identity() {
        let (ledger, root) = test_ledger();
        let initial = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/a.jsonl"),
                    SourceArea::Sessions,
                    21,
                    31,
                    50,
                    1,
                    1,
                ),
                observation(
                    &fixture_path("mu-sessions/b.jsonl"),
                    SourceArea::Sessions,
                    22,
                    32,
                    60,
                    2,
                    1,
                ),
            ]))
            .unwrap();
        let a_id = initial.results[0].source_file_id;
        let b_id = initial.results[1].source_file_id;

        let swapped = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/b.jsonl"),
                    SourceArea::Sessions,
                    21,
                    31,
                    50,
                    1,
                    2,
                ),
                observation(
                    &fixture_path("mu-sessions/a.jsonl"),
                    SourceArea::Sessions,
                    22,
                    32,
                    60,
                    2,
                    2,
                ),
            ]))
            .unwrap();
        assert_eq!(swapped.results[0].source_file_id, a_id);
        assert_eq!(swapped.results[1].source_file_id, b_id);
        for result in &swapped.results {
            assert!(result.moved);
            assert!(!result.replaced);
            assert_eq!(result.file_generation, 1);
            assert!(result.rebuild_consumers.is_empty());
        }

        let connection = Connection::open(ledger.database_path()).unwrap();
        let a_path: String = connection
            .query_row(
                "SELECT current_path FROM source_files WHERE source_file_id = ?1",
                [a_id],
                |row| row.get(0),
            )
            .unwrap();
        let b_path: String = connection
            .query_row(
                "SELECT current_path FROM source_files WHERE source_file_id = ?1",
                [b_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_path, fixture_path("mu-sessions/b.jsonl"));
        assert_eq!(b_path, fixture_path("mu-sessions/a.jsonl"));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn moving_source_and_new_file_at_old_path_are_resolved_as_a_batch() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/a-slot.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                30,
                1,
                1,
            ),
        );

        // Put the new file first to prove matching is based on the complete
        // batch rather than observation iteration order.
        let outcome = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/a-slot.jsonl"),
                    SourceArea::Sessions,
                    42,
                    52,
                    10,
                    2,
                    2,
                ),
                observation(
                    &fixture_path("mu-sessions/b-slot.jsonl"),
                    SourceArea::Sessions,
                    41,
                    51,
                    30,
                    1,
                    2,
                ),
            ]))
            .unwrap();
        let new_source = &outcome.results[0];
        let moved_source = &outcome.results[1];
        assert!(new_source.created);
        assert_ne!(new_source.source_file_id, first.source_file_id);
        assert_eq!(moved_source.source_file_id, first.source_file_id);
        assert!(moved_source.moved);
        assert!(!moved_source.replaced);
        assert_eq!(moved_source.file_generation, 1);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn safe_fact_record_offsets_cannot_exceed_resolved_offset() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/offsets.jsonl"),
                SourceArea::Sessions,
                61,
                71,
                64,
                1,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let project_path = fixture_path("project");
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_files SET thread_id = 'thread-offsets' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 1, committed_offset = 64,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                &format!(
                    "INSERT INTO rollout_metadata_facts (
                    source_file_id, file_generation, metadata_parser_version,
                    resolved_through_offset, owning_thread_id, continuation_state,
                    cwd, cwd_provenance, cwd_record_offset,
                    parent_thread_id_hint, parent_hint_provenance, parent_hint_record_offset,
                    agent_role_hint, agent_role_provenance, agent_role_record_offset,
                    replay_start_offset, owning_records_start_offset,
                    ownership_confidence, fact_quality_status, updated_at_ms
                 ) VALUES (
                    ?1, 1, 1, 64, 'thread-offsets', 'owning_live',
                    '{project_path}', 'session_meta', 1,
                    'parent', 'subagent_source', 2,
                    'worker', 'subagent_source', 3,
                    4, 5, 'confirmed', 'complete', 6
                 )"
                ),
                [source_file_id],
            )
            .unwrap();

        for column in [
            "cwd_record_offset",
            "parent_hint_record_offset",
            "agent_role_record_offset",
            "replay_start_offset",
            "owning_records_start_offset",
        ] {
            connection
                .execute(
                    &format!(
                        "UPDATE rollout_metadata_facts SET {column} = 65 WHERE source_file_id = ?1"
                    ),
                    [source_file_id],
                )
                .unwrap();
            let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
            assert_eq!(
                state.entries[0].safe_fact,
                SafeFactState::Stale(SafeFactMismatchReason::InvalidFact),
                "column {column}"
            );
            connection
                .execute(
                    &format!(
                        "UPDATE rollout_metadata_facts SET {column} = 1 WHERE source_file_id = ?1"
                    ),
                    [source_file_id],
                )
                .unwrap();
        }
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn rebuild_rejects_missing_consumer_checkpoint() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/no-usage.jsonl"),
                SourceArea::Sessions,
                81,
                91,
                10,
                1,
                1,
            ),
        );
        let outcome = ledger.require_checkpoint_rebuild(
            CheckpointRebuildCommand::new(ConsumerKind::Usage, vec![result.source_file_id])
                .unwrap(),
        );
        assert!(outcome.is_err());

        let metadata_status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT processing_status FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [result.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(metadata_status, "pending");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn source_changed_rejects_source_observation_write() {
        let (ledger, root) = test_ledger();
        let changed = Ledger::open(LedgerOptions::new(
            ledger.database_path(),
            root.join("different-codex-home"),
        ))
        .unwrap();
        let outcome = changed.record_source_observations(complete_batch(vec![observation(
            &fixture_path("mu-sessions/rejected.jsonl"),
            SourceArea::Sessions,
            101,
            111,
            10,
            1,
            1,
        )]));
        assert!(outcome.is_err());
        let count: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM source_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(changed);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn unavailable_region_preserves_existing_source_status() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/unavailable.jsonl"),
                SourceArea::Sessions,
                11,
                12,
                20,
                1,
                1,
            ),
        );

        ledger
            .record_source_observations(
                SourceObservationBatch::new(
                    Vec::new(),
                    SourceRegionStatus::Unavailable("PERMISSION_DENIED".to_owned()),
                    SourceRegionStatus::Complete,
                )
                .unwrap(),
            )
            .unwrap();

        let status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT file_status FROM source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "present");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn complete_region_marks_only_unobserved_present_sources_missing() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/complete-missing.jsonl"),
                SourceArea::Sessions,
                13,
                14,
                20,
                1,
                1,
            ),
        );

        ledger
            .record_source_observations(
                SourceObservationBatch::new(
                    Vec::new(),
                    SourceRegionStatus::Complete,
                    SourceRegionStatus::Unavailable("ARCHIVE_UNAVAILABLE".to_owned()),
                )
                .unwrap(),
            )
            .unwrap();

        let status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT file_status FROM source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "missing");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_build_observation_dispositions_are_atomic_and_preserve_required_boundary() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/spec04-a.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                100,
                1_000,
                1,
            ),
        );
        let source1 = first.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "INSERT INTO threads(thread_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root',NULL,'root','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE source_files SET thread_id='root' WHERE source_file_id=?1",
                    [source1],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[source1], 2)
                .unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .record_progress(crate::usage::rebuild::SourceProgress {
                    source_file_id: source1,
                    expected_generation: 1,
                    start_offset: 0,
                    last_complete_offset: 100,
                    observed_raw_size: 100,
                    expected_guard_hash: None,
                    guard_hash: Some(vec![7; 32]),
                    tail: crate::usage::rebuild::TailProof::None,
                    updated_at_ms: 3,
                })
                .unwrap();
        }

        // Same physical generation and raw size retains the completed proof.
        let unchanged = ledger
            .record_source_observations(complete_batch(vec![observation(
                &fixture_path("mu-sessions/spec04-a.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                100,
                1_000,
                4,
            )]))
            .unwrap();
        assert_eq!(
            unchanged.results[0].build_disposition,
            crate::domain::BuildDisposition::Unchanged
        );

        // Same-generation growth invalidates only completion/tail proof. The
        // reader-proven required boundary remains 100 until a reader commit.
        let grown = ledger
            .record_source_observations(complete_batch(vec![observation(
                &fixture_path("mu-sessions/spec04-a.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                120,
                1_001,
                5,
            )]))
            .unwrap();
        assert_eq!(
            grown.results[0].build_disposition,
            crate::domain::BuildDisposition::CompletionInvalidated
        );
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let proof: (i64, i64, String, String, i64) = connection
                .query_row(
                    "SELECT b.required_through_offset,b.observed_raw_size,b.raw_tail_status,
                            b.completion_status,c.committed_offset
                     FROM usage_build_sources b JOIN source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source1],
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
            assert_eq!(
                proof,
                (100, 120, "unverified".into(), "pending".into(), 100)
            );
        }

        // A new present file is added to the frozen manifest in the same
        // source-observation transaction and receives the mandatory from-zero
        // usage checkpoint.
        let added = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/spec04-a.jsonl"),
                    SourceArea::Sessions,
                    41,
                    51,
                    120,
                    1_001,
                    6,
                ),
                observation(
                    &fixture_path("mu-sessions/spec04-b.jsonl"),
                    SourceArea::Sessions,
                    42,
                    52,
                    30,
                    2_000,
                    6,
                ),
            ]))
            .unwrap();
        let source2_result = added.results.iter().find(|r| r.created).unwrap();
        assert_eq!(
            source2_result.build_disposition,
            crate::domain::BuildDisposition::MemberAdded
        );
        let source2 = source2_result.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let row: (i64, i64, String, i64, String) = connection
                .query_row(
                    "SELECT b.required_through_offset,b.observed_raw_size,b.raw_tail_status,
                            c.committed_offset,c.processing_status
                     FROM usage_build_sources b JOIN source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source2],
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
            assert_eq!(
                row,
                (0, 30, "unverified".into(), 0, "rebuild_required".into())
            );
        }

        // Carry-in-progress reappearance under the same frozen identity keeps
        // its cursor and is reported distinctly; source observation must not
        // restore the checkpoint early.
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "UPDATE source_files SET file_status='missing' WHERE source_file_id=?1",
                    [source2],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE usage_build_sources SET carry_from_epoch=0,carry_phase='occurrences',
                         completion_status='pending',completion_error_code=NULL,carry_after_start_offset=7
                     WHERE build_epoch=1 AND source_file_id=?1",
                    [source2],
                )
                .unwrap();
        }
        let resumed = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/spec04-a.jsonl"),
                    SourceArea::Sessions,
                    41,
                    51,
                    120,
                    1_001,
                    7,
                ),
                observation(
                    &fixture_path("mu-sessions/spec04-b.jsonl"),
                    SourceArea::Sessions,
                    42,
                    52,
                    30,
                    2_000,
                    7,
                ),
            ]))
            .unwrap();
        let resumed2 = resumed
            .results
            .iter()
            .find(|r| r.source_file_id == source2)
            .unwrap();
        assert_eq!(
            resumed2.build_disposition,
            crate::domain::BuildDisposition::CarryResumedPresent
        );
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let row: (String, Option<i64>, String, i64) = connection
                .query_row(
                    "SELECT carry_phase,carry_after_start_offset,c.processing_status,c.committed_offset
                     FROM usage_build_sources b JOIN source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source2],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                row,
                ("occurrences".into(), Some(7), "rebuild_required".into(), 0)
            );
        }

        // Physical replacement invokes the shared replacement protocol. The
        // affected source restarts at zero while the other old manifest member
        // and its required boundary remain present.
        let replaced = ledger
            .record_source_observations(complete_batch(vec![
                observation(
                    &fixture_path("mu-sessions/spec04-a.jsonl"),
                    SourceArea::Sessions,
                    141,
                    151,
                    20,
                    3_000,
                    8,
                ),
                observation(
                    &fixture_path("mu-sessions/spec04-b.jsonl"),
                    SourceArea::Sessions,
                    42,
                    52,
                    30,
                    2_000,
                    8,
                ),
            ]))
            .unwrap();
        let replaced1 = replaced
            .results
            .iter()
            .find(|r| r.current_generation() == 2)
            .unwrap();
        assert_eq!(
            replaced1.build_disposition,
            crate::domain::BuildDisposition::Replaced
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        let ids = connection
            .prepare("SELECT source_file_id FROM usage_build_sources WHERE build_epoch=1 ORDER BY source_file_id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec![source1, source2]);
        let affected: (i64, i64, String) = connection
            .query_row(
                "SELECT b.required_generation,c.committed_offset,c.processing_status
                 FROM usage_build_sources b JOIN source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=?1",
                [source1],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(affected, (2, 0, "rebuild_required".into()));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_carry_present_active_prefix_guard_is_decided_inside_observation_transaction() {
        fn prepare() -> (Ledger, std::path::PathBuf, i64) {
            let (ledger, root) = test_ledger();
            let first = one_observation(
                &ledger,
                observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    1,
                ),
            );
            let source = first.source_file_id;
            {
                let connection = Connection::open(ledger.database_path()).unwrap();
                connection
                    .execute_batch(
                        "INSERT INTO threads(thread_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root',NULL,'root','main','unknown',0,'complete',1);",
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET thread_id='root' WHERE source_file_id=?1",
                        [source],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO source_checkpoints(source_file_id,consumer_kind,parser_version,
                            committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code)
                         VALUES (?1,'usage',?2,10,?3,'ready',1,NULL)",
                        rusqlite::params![source, crate::usage::USAGE_PARSER_VERSION, vec![9_u8; 32]],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO usage_source_states(
                            ledger_epoch,source_file_id,file_generation,device_id,inode,
                            usage_parser_version,canonical_algorithm_version,resolved_through_offset,
                            observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,
                            root_session_id,continuation_state,chain_state,chain_block_reason,updated_at_ms)
                         VALUES (1,?1,1,61,71,?2,?3,10,10,'none',NULL,'root','root',
                                 'owning_live','continuous',NULL,1)",
                        rusqlite::params![
                            source,
                            crate::usage::USAGE_PARSER_VERSION,
                            crate::usage::USAGE_CANONICAL_ALGORITHM_VERSION,
                        ],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1,
                            usage_build_epoch=NULL,usage_build_parser_version=NULL WHERE id=1",
                        [crate::usage::USAGE_PARSER_VERSION],
                    )
                    .unwrap();
            }
            {
                let mut connection = ledger.connection().unwrap();
                crate::usage::rebuild::RebuildLedger::new(&mut connection)
                    .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[source], 2)
                    .unwrap();
            }
            {
                let connection = Connection::open(ledger.database_path()).unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET file_status='missing' WHERE source_file_id=?1",
                        [source],
                    )
                    .unwrap();
            }
            ledger.begin_usage_carry(source, 3).unwrap();
            (ledger, root, source)
        }

        let (ledger, root, source) = prepare();
        let requirement = ledger.load_usage_carry_observation_requirements().unwrap();
        assert_eq!(requirement.len(), 1);
        assert_eq!(requirement[0].active_committed_offset, 10);
        assert_eq!(requirement[0].active_guard_hash, Some(vec![9_u8; 32]));

        let outcome = ledger
            .record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    4,
                )]),
                &[UsageCarryObservationProof {
                    device_id: 61,
                    inode: 71,
                    active_committed_offset: 10,
                    guard_matches: false,
                }],
            )
            .unwrap();
        assert_eq!(
            outcome.results[0].build_disposition,
            crate::domain::BuildDisposition::Replaced
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        let row: (String, Option<i64>, String, i64, i64) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,c.processing_status,c.committed_offset,
                        (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=2 AND source_file_id=?1)
                 FROM usage_build_sources b JOIN source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(row, ("none".into(), None, "rebuild_required".into(), 0, 0));
        drop(connection);
        drop(ledger);
        cleanup(root);

        let (ledger, root, source) = prepare();
        let outcome = ledger
            .record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    4,
                )]),
                &[UsageCarryObservationProof {
                    device_id: 61,
                    inode: 71,
                    active_committed_offset: 10,
                    guard_matches: true,
                }],
            )
            .unwrap();
        assert_eq!(
            outcome.results[0].build_disposition,
            crate::domain::BuildDisposition::CarryResumedPresent
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        let row: (String, String, i64) = connection
            .query_row(
                "SELECT b.carry_phase,c.processing_status,c.committed_offset
                 FROM usage_build_sources b JOIN source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("occurrences".into(), "rebuild_required".into(), 0));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_source_observation_and_build_replacement_roll_back_together() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/spec04-atomic.jsonl"),
                SourceArea::Sessions,
                61,
                71,
                90,
                1_000,
                1,
            ),
        );
        let source_id = first.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "INSERT INTO threads(thread_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root',NULL,'root','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE source_files SET thread_id='root' WHERE source_file_id=?1",
                    [source_id],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[source_id], 2)
                .unwrap();
        }
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_spec04_source_build_replace
                     BEFORE DELETE ON usage_build_sources
                     BEGIN
                       SELECT RAISE(ABORT, 'injected build replacement failure');
                     END;",
                )
                .unwrap();
        }

        let failed = ledger.record_source_observations(complete_batch(vec![observation(
            &fixture_path("mu-sessions/spec04-atomic.jsonl"),
            SourceArea::Sessions,
            161,
            171,
            20,
            2_000,
            3,
        )]));
        assert!(failed.is_err());
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let source: (i64, i64, i64, i64, String) = connection
                .query_row(
                    "SELECT device_id,inode,file_generation,observed_size,file_status
                     FROM source_files WHERE source_file_id=?1",
                    [source_id],
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
            assert_eq!(source, (61, 71, 1, 90, "present".into()));
            let build: (i64, i64, String) = connection
                .query_row(
                    "SELECT expected_file_generation,required_through_offset,completion_status
                     FROM usage_build_sources WHERE build_epoch=1 AND source_file_id=?1",
                    [source_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(build, (1, 0, "pending".into()));
            let checkpoint: (i64, String) = connection
                .query_row(
                    "SELECT committed_offset,processing_status FROM source_checkpoints
                     WHERE source_file_id=?1 AND consumer_kind='usage'",
                    [source_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(checkpoint, (0, "rebuild_required".into()));
            connection
                .execute_batch("DROP TRIGGER fail_spec04_source_build_replace;")
                .unwrap();
        }
        drop(ledger);
        cleanup(root);
    }
}
