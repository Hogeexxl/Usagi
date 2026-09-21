//! Atomic metadata fact, checkpoint, normalized Thread, and usage-reconcile writes.
//!
//! SQL stays private behind `Ledger`. The metadata commit transaction also
//! performs the Spec04 active-usage root reconciliation and any required
//! shadow-build replacement before it commits.

use crate::codex::domain::{
    AgentPathProvenance, AgentRoleProvenance, CheckpointProcessingStatus, CommitOutcome,
    ContinuationState, CwdProvenance, FactQualityStatus, FileStatus, MetadataCheckpointAdvance,
    MetadataCheckpointState, MetadataCommitBatch, MetadataSourceCommit, MetadataThreadCommit,
    ParentHintProvenance, RolloutMetadataFact, SourceArea, SourceFileState,
};
use crate::domain::{
    AgentRole, ExistingThreadProjection, MetadataQualityStatus, Patch, ProjectKind,
    ResolvedThreadPatch,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{CodexStorage, CodexStorageError};
use crate::storage::{Result as StorageResult, StorageError};

/// Commit a batch one Thread group at a time.
///
/// A group is the unit of isolation: every source binding, safe fact,
/// metadata checkpoint, and optional normalized Thread patch in that group is
/// committed by one `BEGIN IMMEDIATE` transaction.  Groups which precede a
/// later failing group remain committed by design.
pub(super) fn load_existing_threads(
    storage: &CodexStorage<'_>,
) -> Result<Vec<ExistingThreadProjection>, CodexStorageError> {
    storage.with_read(|connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(CodexStorageError::from)?;
        let mut statement = transaction.prepare(
            "SELECT
                thread_id, source, native_session_id, parent_thread_id, root_session_id, agent_role,
                title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status
             FROM threads ORDER BY thread_id",
        )?;
        let mut rows = statement.query([])?;
        let mut projections = Vec::new();
        while let Some(row) = rows.next()? {
            let agent_role: String = row.get(5)?;
            let agent_role = AgentRole::try_from(agent_role.as_str()).map_err(|error| {
                CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
            })?;
            let archived: i64 = row.get(13)?;
            let archived = match archived {
                0 => false,
                1 => true,
                other => {
                    return Err(CodexStorageError::Storage(StorageError::invalid_state(
                        format!("invalid archived value {other}"),
                    )));
                }
            };
            let quality: String = row.get(14)?;
            let metadata_quality_status = MetadataQualityStatus::try_from(quality.as_str())
                .map_err(|error| {
                    CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
                })?;
            let project_kind: String = row.get(9)?;
            let project_kind = ProjectKind::try_from(project_kind.as_str()).map_err(|error| {
                CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
            })?;
            projections.push(ExistingThreadProjection {
                thread_id: row.get(0)?,
                source: row.get::<_, String>(1)?.parse().map_err(|error| {
                    rusqlite::Error::InvalidParameterName(format!("invalid source: {error}"))
                })?,
                native_session_id: row.get(2)?,
                parent_thread_id: row.get(3)?,
                root_session_id: row.get(4)?,
                agent_role,
                title: row.get(6)?,
                project_name: row.get(7)?,
                project_path: row.get(8)?,
                project_kind,
                metadata_model: row.get(10)?,
                created_at_ms: row.get(11)?,
                updated_at_ms: row.get(12)?,
                archived,
                metadata_quality_status,
            });
        }
        drop(rows);
        drop(statement);
        transaction.commit().map_err(CodexStorageError::from)?;
        Ok(projections)
    })
}

pub(super) fn commit_metadata(
    storage: &CodexStorage<'_>,
    batch: MetadataCommitBatch,
) -> Result<CommitOutcome, CodexStorageError> {
    batch.validate().map_err(|error| {
        CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
    })?;
    let mut data_changed = false;
    let mut data_revision = None;

    for group in &batch.groups {
        let mut source_tx = storage.begin_write_txn()?;
        let changed = metadata_group(&mut source_tx, group)?;
        if changed {
            data_changed = true;
        }
        source_tx.commit()?;
        let revision = storage.with_read(|connection| {
            connection
                .query_row(
                    "SELECT data_revision FROM app_meta WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(CodexStorageError::from)
        })?;
        data_revision = Some(revision);
    }

    let data_revision = data_revision.ok_or_else(|| {
        CodexStorageError::Storage(StorageError::invalid_state(
            "metadata commit contained no groups",
        ))
    })?;
    CommitOutcome::new(batch.groups.len(), data_revision, data_changed)
        .map_err(|error| CodexStorageError::Storage(StorageError::invalid_state(error.to_string())))
}

fn metadata_group(
    source_tx: &mut super::CodexWriteTxn<'_>,
    group: &MetadataThreadCommit,
) -> Result<bool, CodexStorageError> {
    let current_thread = source_tx.with_private_state(|transaction| {
        read_thread(transaction, &group.thread_id).map_err(CodexStorageError::from)
    })?;
    let next_thread = group
        .resolved_patch
        .as_ref()
        .map(|patch| match current_thread.as_ref() {
            Some(current) => apply_existing_patch(current, patch),
            None => apply_new_patch(patch),
        })
        .transpose()
        .map_err(CodexStorageError::from)?;
    if let Some(next_thread) = next_thread.as_ref() {
        source_tx.with_private_state(|transaction| {
            validate_thread_relationships(transaction, next_thread).map_err(CodexStorageError::from)
        })?;
    }
    if let Some(patch) = group.resolved_patch.as_ref() {
        let identity = crate::domain::SessionIdentity::new(
            &group.thread_id,
            patch.source.clone(),
            &patch.native_session_id,
        )
        .map_err(|error| {
            CodexStorageError::Storage(StorageError::invalid_state(error.to_string()))
        })?;
        source_tx.upsert_session_metadata_no_revision(&identity, patch)?;
    }
    let changed = commit_group(
        source_tx,
        group,
        current_thread.as_ref(),
        next_thread.as_ref(),
    )?;
    if changed {
        source_tx.bump_data_revision()?;
    }
    Ok(changed)
}

fn commit_group(
    source_tx: &mut super::CodexWriteTxn<'_>,
    group: &MetadataThreadCommit,
    current_thread: Option<&ThreadRow>,
    next_thread: Option<&ThreadRow>,
) -> Result<bool, CodexStorageError> {
    // Read and validate every source precondition first.  A later source
    // failure must not leave an earlier source in this group bound or advanced.
    let (sources, binding_changed_source_ids) = source_tx.with_private_state(|transaction| {
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
        Ok::<_, StorageError>((sources, binding_changed_source_ids))
    })?;

    if let (Some(patch), Some(next_thread)) = (&group.resolved_patch, next_thread) {
        super::usage::reconcile_usage_metadata_change(
            source_tx,
            &group.thread_id,
            current_thread
                .as_ref()
                .and_then(|thread| thread.root_session_id.as_deref()),
            next_thread.root_session_id.as_deref(),
            &binding_changed_source_ids,
        )?;
        source_tx.with_private_state(|transaction| {
            verify_patch_postcondition(transaction, patch, next_thread)
        })?;
    } else if !binding_changed_source_ids.is_empty() {
        // A source can become bound to an already-existing Thread without a
        // Thread patch.  Spec04 still requires the frozen build binding proof
        // to be reconciled in this same metadata transaction.
        let root = current_thread
            .as_ref()
            .and_then(|thread| thread.root_session_id.as_deref());
        super::usage::reconcile_usage_metadata_change(
            source_tx,
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
        .zip(next_thread)
        .is_some_and(|(_, next)| current_thread != Some(next));

    // A write-after-read check protects the key cross-table equalities from
    // schema changes and direct SQL interference.  It is still inside the
    // transaction, so any mismatch rolls the entire group back.
    source_tx.with_private_state(|transaction| {
        for source_commit in &group.sources {
            verify_source_postcondition(transaction, group, source_commit)?;
        }
        Ok::<_, StorageError>(())
    })?;

    Ok(stable_changed)
}

fn read_data_revision(transaction: &Connection) -> StorageResult<i64> {
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
    source: crate::source::SourceId,
    native_session_id: String,
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
    metadata_quality_status: MetadataQualityStatus,
    metadata_resolved_at_ms: i64,
}

fn read_thread(transaction: &Connection, thread_id: &str) -> StorageResult<Option<ThreadRow>> {
    transaction
        .query_row(
            "SELECT
                thread_id, source, native_session_id, parent_thread_id, root_session_id, agent_role,
                title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status,
                metadata_resolved_at_ms
             FROM threads WHERE thread_id = ?1",
            [thread_id],
            |row| {
                let agent_role: String = row.get(5)?;
                let agent_role =
                    AgentRole::try_from(agent_role.as_str()).map_err(super::to_domain_sql_error)?;
                let archived: i64 = row.get(13)?;
                let archived = match archived {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "invalid archived value {other}"
                        )));
                    }
                };
                let quality: String = row.get(14)?;
                let quality = MetadataQualityStatus::try_from(quality.as_str())
                    .map_err(super::to_domain_sql_error)?;
                let project_kind: String = row.get(9)?;
                let project_kind = ProjectKind::try_from(project_kind.as_str())
                    .map_err(super::to_domain_sql_error)?;
                Ok(ThreadRow {
                    thread_id: row.get(0)?,
                    source: row.get::<_, String>(1)?.parse().map_err(|error| {
                        rusqlite::Error::InvalidParameterName(format!("invalid source: {error}"))
                    })?,
                    native_session_id: row.get(2)?,
                    parent_thread_id: row.get(3)?,
                    root_session_id: row.get(4)?,
                    agent_role,
                    title: row.get(6)?,
                    project_name: row.get(7)?,
                    project_path: row.get(8)?,
                    project_kind,
                    metadata_model: row.get(10)?,
                    created_at_ms: row.get(11)?,
                    updated_at_ms: row.get(12)?,
                    archived,
                    metadata_quality_status: quality,
                    metadata_resolved_at_ms: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn read_source(transaction: &Connection, source_file_id: i64) -> StorageResult<SourceFileState> {
    let source = transaction
        .query_row(
            "SELECT
                source_file_id, thread_id, current_path, source_area,
                device_id, inode, file_generation, observed_size,
                observed_mtime_ns, file_status, last_seen_at_ms
             FROM codex_source_files WHERE source_file_id = ?1",
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
                let processing_status =
                    CheckpointProcessingStatus::try_from(processing_status.as_str())
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
    transaction: &Connection,
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
             FROM codex_rollout_metadata_facts WHERE source_file_id = ?1",
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
                let ownership =
                    crate::codex::domain::OwnershipConfidence::try_from(ownership.as_str())
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
    transaction: &Connection,
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
    transaction: &Connection,
    group: &MetadataThreadCommit,
    source_commit: &MetadataSourceCommit,
    source: &SourceFileState,
) -> StorageResult<()> {
    let changed = transaction.execute(
        "UPDATE codex_source_files
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

fn write_fact(transaction: &Connection, fact: &RolloutMetadataFact) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_rollout_metadata_facts (
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
    transaction: &Connection,
    source_file_id: i64,
    advance: &MetadataCheckpointAdvance,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_source_checkpoints (
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
    if patch.source != current.source || patch.native_session_id != current.native_session_id {
        return Err(StorageError::invalid_state(format!(
            "Thread {} canonical identity is immutable",
            patch.thread_id
        )));
    }
    if patch.resolved_at_ms < current.metadata_resolved_at_ms {
        return Err(StorageError::invalid_state(format!(
            "patch for Thread {} is older than metadata_resolved_at_ms",
            patch.thread_id
        )));
    }
    let next = ThreadRow {
        thread_id: current.thread_id.clone(),
        source: current.source.clone(),
        native_session_id: current.native_session_id.clone(),
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
        metadata_quality_status: patch.metadata_quality_status,
        metadata_resolved_at_ms: patch.resolved_at_ms,
    };
    validate_thread_row(&next)?;
    Ok(next)
}

fn apply_new_patch(patch: &ResolvedThreadPatch) -> StorageResult<ThreadRow> {
    let mut next = ThreadRow {
        thread_id: patch.thread_id.clone(),
        source: patch.source.clone(),
        native_session_id: patch.native_session_id.clone(),
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
    thread
        .source
        .validate()
        .map_err(|error| StorageError::invalid_state(error.to_string()))?;
    if thread.native_session_id.trim().is_empty() {
        return Err(StorageError::invalid_state(
            "thread native_session_id must not be empty",
        ));
    }
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

fn validate_thread_relationships(
    transaction: &Connection,
    thread: &ThreadRow,
) -> StorageResult<()> {
    for related_id in [
        thread.parent_thread_id.as_deref(),
        thread.root_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let related_source: Option<String> = transaction
            .query_row(
                "SELECT source FROM threads WHERE thread_id=?1",
                [related_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(related_source) = related_source
            && related_source != thread.source.as_str()
        {
            return Err(StorageError::invalid_state(format!(
                "Thread {} parent/root source mismatch",
                thread.thread_id
            )));
        }
    }
    Ok(())
}

fn verify_patch_postcondition(
    transaction: &Connection,
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
    transaction: &Connection,
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
#[path = "metadata/tests.rs"]
mod tests;
