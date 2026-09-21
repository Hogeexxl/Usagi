//! Physical rollout observations and independent consumer checkpoints.
//!
//! This module owns the Spec 01 physical-source observation transaction.
//! Spec 04 extends that same transaction with usage-build manifest transitions
//! so no cross-table crash window can exist. Rollout contents are still never
//! parsed here.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::codex::domain::{
    AgentPathProvenance, AgentRoleProvenance, CheckpointOutcome, CheckpointProcessingStatus,
    CheckpointRebuildCommand, ConsumerKind, ContinuationState, CwdProvenance, FactQualityStatus,
    FileStatus, MetadataCheckpointState, MetadataScanState, MetadataScanStateEntry,
    OwnershipConfidence, ParentHintProvenance, RolloutMetadataFact, SafeFactMismatchReason,
    SafeFactState, SourceArea, SourceFileState, SourceObservationBatch, SourceObservationResult,
    SourceOutcome,
};

use super::{CodexStorage, CodexStorageError};
use crate::storage::{Result as StorageResult, StorageError};

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

pub(super) fn load_usage_carry_observation_requirements(
    storage: &CodexStorage<'_>,
) -> Result<Vec<UsageCarryObservationRequirement>, CodexStorageError> {
    storage.with_read(|connection| {
        let build_epoch: Option<i64> = connection
            .query_row(
                "SELECT build_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| row.get(0),
            )
            .map_err(CodexStorageError::from)?;
        let Some(build_epoch) = build_epoch else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare(
                "SELECT b.expected_device_id,b.expected_inode,b.active_committed_offset,b.active_guard_hash
                 FROM codex_usage_build_sources b
                 WHERE b.build_epoch=?1 AND b.carry_phase<>'none'
                 ORDER BY b.source_file_id",
            )
            .map_err(CodexStorageError::from)?;
        let rows = statement
            .query_map([build_epoch], |row| {
                Ok(UsageCarryObservationRequirement {
                    device_id: row.get(0)?,
                    inode: row.get(1)?,
                    active_committed_offset: row.get(2)?,
                    active_guard_hash: row.get(3)?,
                })
            })
            .map_err(CodexStorageError::from)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(CodexStorageError::from)
    })
}

pub(super) fn record_source_observations(
    storage: &CodexStorage<'_>,
    batch: SourceObservationBatch,
    usage_carry_proofs: &[UsageCarryObservationProof],
) -> Result<SourceOutcome, CodexStorageError> {
    batch.validate().map_err(|error| {
        CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
    })?;
    let mut tx = storage.begin_write_txn()?;
    let outcome = tx.with_private_state(|connection| {
        source_observations_private(connection, &batch, usage_carry_proofs)
            .map_err(CodexStorageError::from)
    })?;
    if let Some(build_epoch) = tx.usage_epoch_state()?.build_epoch {
        crate::codex::storage::rebuild::delete_orphan_build_events(&mut tx, build_epoch)?;
    }
    tx.commit()?;
    Ok(outcome)
}

pub(super) fn load_metadata_scan_state(
    storage: &CodexStorage<'_>,
    source_file_ids: &[i64],
) -> Result<MetadataScanState, CodexStorageError> {
    validate_ids(source_file_ids).map_err(CodexStorageError::from)?;
    if source_file_ids.is_empty() {
        return MetadataScanState::new(Vec::new()).map_err(|error| {
            CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
        });
    }
    storage.with_read(|connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(CodexStorageError::from)?;
        let mut entries = Vec::with_capacity(source_file_ids.len());
        let mut seen = HashSet::with_capacity(source_file_ids.len());
        for source_file_id in source_file_ids {
            if !seen.insert(*source_file_id) {
                return Err(CodexStorageError::Storage(invalid_state(
                    "duplicate source_file_id in metadata scan state",
                )));
            }
            let source = query_source_state(&transaction, *source_file_id)
                .map_err(CodexStorageError::from)?
                .ok_or_else(|| {
                    CodexStorageError::Storage(invalid_state(format!(
                        "source file {source_file_id} does not exist"
                    )))
                })?;
            let checkpoint = query_metadata_checkpoint(&transaction, *source_file_id)
                .map_err(CodexStorageError::from)?;
            let fact = query_metadata_fact(&transaction, *source_file_id)
                .map_err(CodexStorageError::from)?;
            let safe_fact = classify_safe_fact(&source, checkpoint.as_ref(), fact);
            entries.push(MetadataScanStateEntry {
                source,
                metadata_checkpoint: checkpoint,
                safe_fact,
            });
        }
        transaction.commit().map_err(CodexStorageError::from)?;
        MetadataScanState::new(entries).map_err(|error| {
            CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
        })
    })
}

pub(super) fn require_checkpoint_rebuild(
    storage: &CodexStorage<'_>,
    command: CheckpointRebuildCommand,
) -> Result<CheckpointOutcome, CodexStorageError> {
    command.validate().map_err(|error| {
        CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
    })?;
    let mut tx = storage.begin_write_txn()?;
    let outcome = tx.with_private_state(|connection| {
        checkpoint_rebuild_private(connection, &command).map_err(CodexStorageError::from)
    })?;
    tx.commit()?;
    Ok(outcome)
}

fn source_observations_private(
    transaction: &Connection,
    batch: &SourceObservationBatch,
    usage_carry_proofs: &[UsageCarryObservationProof],
) -> StorageResult<SourceOutcome> {
    let existing = load_existing_sources(transaction)?;
    let plans = plan_observations(&existing, batch)?;

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
                "UPDATE codex_source_files SET current_path = ?2 WHERE source_file_id = ?1",
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
                .ok_or_else(|| invalid_state("observation plan references an unknown source"))?;
            (
                old.thread_id.clone(),
                Some(old.current_path.clone()),
                Some(old.source_area),
            )
        };

        if plan.created {
            transaction.execute(
                "INSERT INTO codex_source_files (
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
                "INSERT INTO codex_source_checkpoints (
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
                build_disposition: crate::codex::domain::BuildDisposition::Unchanged,
            });
            continue;
        }

        let moved = old_path.as_deref() != Some(observation.current_path.as_str())
            || old_area != Some(observation.source_area);
        let replaced = plan.replaced;

        transaction.execute(
            "UPDATE codex_source_files SET
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
                "DELETE FROM codex_rollout_metadata_facts WHERE source_file_id = ?1",
                [plan.source_file_id],
            )?;
            let mut checkpoint_rows = transaction.prepare(
                "SELECT consumer_kind FROM codex_source_checkpoints
                 WHERE source_file_id = ?1 ORDER BY consumer_kind",
            )?;
            let consumers = checkpoint_rows
                .query_map([plan.source_file_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(checkpoint_rows);
            for consumer in consumers {
                rebuild_consumers
                    .push(ConsumerKind::try_from(consumer.as_str()).map_err(domain_sql_error)?);
            }
            transaction.execute(
                "UPDATE codex_source_checkpoints SET
                    committed_offset = 0,
                    guard_hash = NULL,
                    processing_status = 'rebuild_required',
                    last_successful_scan_at_ms = NULL,
                    last_error_code = NULL
                 WHERE source_file_id = ?1",
                [plan.source_file_id],
            )?;
        }

        transaction.execute(
            "INSERT OR IGNORE INTO codex_source_checkpoints (
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
            build_disposition: crate::codex::domain::BuildDisposition::Unchanged,
        });
    }

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
                "UPDATE codex_source_files SET file_status = 'missing'
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
    crate::codex::storage::rebuild::apply_source_observations_to_build_tx(
        transaction,
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

    let _ = temporary_paths;
    SourceOutcome::new(results).map_err(|error| StorageError::invalid_state(error.to_string()))
}

fn checkpoint_rebuild_private(
    transaction: &Connection,
    command: &CheckpointRebuildCommand,
) -> StorageResult<CheckpointOutcome> {
    for source_file_id in &command.source_file_ids {
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT source_file_id FROM codex_source_files WHERE source_file_id = ?1",
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
            "UPDATE codex_source_checkpoints SET
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

    Ok(CheckpointOutcome {
        consumer_kind: command.consumer_kind,
        source_file_ids: command.source_file_ids.clone(),
    })
}

fn plan_observations(
    existing: &[ExistingSource],
    batch: &SourceObservationBatch,
) -> StorageResult<Vec<ObservationPlan>> {
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

fn load_existing_sources(transaction: &Connection) -> StorageResult<Vec<ExistingSource>> {
    let mut statement = transaction.prepare(
        "SELECT source_file_id, thread_id, current_path, source_area,
                device_id, inode, file_generation, observed_size,
                observed_mtime_ns, file_status
         FROM codex_source_files",
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
    transaction: &Connection,
    source_file_id: i64,
) -> StorageResult<Option<SourceFileState>> {
    transaction
        .query_row(
            "SELECT source_file_id, thread_id, current_path, source_area,
                    device_id, inode, file_generation, observed_size,
                    observed_mtime_ns, file_status, last_seen_at_ms
             FROM codex_source_files WHERE source_file_id = ?1",
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
    transaction: &Connection,
    source_file_id: i64,
) -> StorageResult<Option<MetadataCheckpointState>> {
    transaction
        .query_row(
            "SELECT parser_version, committed_offset, guard_hash,
                    processing_status, last_successful_scan_at_ms, last_error_code
             FROM codex_source_checkpoints
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
    transaction: &Connection,
    source_file_id: i64,
) -> StorageResult<Option<RolloutMetadataFact>> {
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
             FROM codex_rollout_metadata_facts WHERE source_file_id = ?1",
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

fn validate_ids(ids: &[i64]) -> StorageResult<()> {
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
#[path = "source_state/tests.rs"]
mod tests;
