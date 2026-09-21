//! Codex private persistence bound to a source transaction.

use std::fmt;

use rusqlite::Connection;

use crate::{
    codex::domain::{
        CheckpointOutcome, CheckpointRebuildCommand, CommitOutcome, MetadataCommitBatch,
        MetadataScanState, SourceObservationBatch, SourceOutcome,
    },
    domain::ExistingThreadProjection,
    source::{CanonicalUsageEventWrite, SourceStorage, SourceStorageError, SourceWriteTxn},
    storage::{StorageError, StorageErrorKind},
};

pub(crate) mod binding;
pub(crate) mod metadata;
pub(crate) mod rebuild;
pub(crate) mod source_state;
pub(crate) mod usage;

pub(crate) use binding::{CodexBindingOutcome, CodexBindingStatus};

#[derive(Debug)]
pub(crate) enum CodexStorageError {
    SourceMismatch,
    BindingUnbound,
    BindingSourceChanged,
    InvalidBindingState,
    Source(SourceStorageError),
    Storage(StorageError),
    Sqlite(rusqlite::Error),
}

impl From<SourceStorageError> for CodexStorageError {
    fn from(error: SourceStorageError) -> Self {
        Self::Source(error)
    }
}

impl From<StorageError> for CodexStorageError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<rusqlite::Error> for CodexStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl fmt::Display for CodexStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for CodexStorageError {}

impl CodexStorageError {
    pub(crate) fn requires_rebuild(&self) -> bool {
        matches!(self, Self::Storage(error) if error.requires_usage_rebuild())
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::BindingUnbound => "SOURCE_UNBOUND",
            Self::BindingSourceChanged => "SOURCE_CHANGED",
            Self::SourceMismatch | Self::InvalidBindingState => "CODEX_SOURCE_BINDING_FAILED",
            Self::Source(SourceStorageError::Storage(_)) | Self::Storage(_) | Self::Sqlite(_) => {
                "CODEX_SOURCE_STORAGE_FAILED"
            }
            Self::Source(SourceStorageError::TransactionClosed)
            | Self::Source(SourceStorageError::TransactionPoisoned)
            | Self::Source(SourceStorageError::SourceMismatch)
            | Self::Source(SourceStorageError::InvalidRequest(_))
            | Self::Source(SourceStorageError::NotImplemented)
            | Self::Source(SourceStorageError::UnsupportedOperation(_)) => {
                "CODEX_SOURCE_BINDING_FAILED"
            }
        }
    }
}

pub(super) fn to_domain_sql_error(error: crate::domain::DomainError) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

pub(crate) struct CodexStorage<'a> {
    storage: &'a SourceStorage,
}

impl<'a> CodexStorage<'a> {
    pub(crate) fn new(storage: &'a SourceStorage) -> Result<Self, CodexStorageError> {
        if storage.source() != &crate::source::SourceId::CODEX {
            return Err(CodexStorageError::SourceMismatch);
        }
        Ok(Self { storage })
    }

    pub(crate) fn begin_write_txn(&self) -> Result<CodexWriteTxn<'_>, CodexStorageError> {
        Ok(CodexWriteTxn {
            inner: self.storage.begin_write_txn()?,
        })
    }

    pub(crate) fn with_read<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, CodexStorageError>,
    ) -> Result<T, CodexStorageError> {
        self.require_ready()?;
        self.storage.with_private_read(operation)
    }

    pub(crate) fn require_ready(&self) -> Result<(), CodexStorageError> {
        let state: (Option<String>, String) = self.storage.with_private_read(|connection| {
            connection
                .query_row(
                    "SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(CodexStorageError::from)
        })?;
        match (state.0, state.1.as_str()) {
            (None, "unbound") => Err(CodexStorageError::BindingUnbound),
            (Some(_), "ready") => Ok(()),
            (Some(_), "source_changed") => Err(CodexStorageError::BindingSourceChanged),
            _ => Err(CodexStorageError::InvalidBindingState),
        }
    }

    pub(crate) fn storage(&self) -> &SourceStorage {
        self.storage
    }

    // The remaining business methods are implemented in the private storage
    // modules.  Their signatures are the frozen A↔B surface.
    pub(crate) fn load_usage_carry_observation_requirements(
        &self,
    ) -> Result<Vec<source_state::UsageCarryObservationRequirement>, CodexStorageError> {
        self.require_ready()?;
        source_state::load_usage_carry_observation_requirements(self)
    }

    pub(crate) fn record_source_observations_with_usage_carry_proofs(
        &self,
        batch: SourceObservationBatch,
        proofs: &[source_state::UsageCarryObservationProof],
    ) -> Result<SourceOutcome, CodexStorageError> {
        self.require_ready()?;
        source_state::record_source_observations(self, batch, proofs)
    }

    pub(crate) fn load_metadata_scan_state(
        &self,
        source_file_ids: &[i64],
    ) -> Result<MetadataScanState, CodexStorageError> {
        self.require_ready()?;
        source_state::load_metadata_scan_state(self, source_file_ids)
    }

    pub(crate) fn require_checkpoint_rebuild(
        &self,
        command: CheckpointRebuildCommand,
    ) -> Result<CheckpointOutcome, CodexStorageError> {
        self.require_ready()?;
        source_state::require_checkpoint_rebuild(self, command)
    }

    pub(crate) fn load_existing_threads(
        &self,
    ) -> Result<Vec<ExistingThreadProjection>, CodexStorageError> {
        self.require_ready()?;
        metadata::load_existing_threads(self)
    }

    pub(crate) fn commit_metadata(
        &self,
        batch: MetadataCommitBatch,
    ) -> Result<CommitOutcome, CodexStorageError> {
        self.require_ready()?;
        metadata::commit_metadata(self, batch)
    }

    pub(crate) fn load_usage_work_list(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
    ) -> Result<usage::UsageWorkList, CodexStorageError> {
        self.require_ready()?;
        usage::load_usage_work_list(self, source_file_ids, parser_version)
    }

    pub(crate) fn load_usage_scan_state(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
    ) -> Result<usage::UsageScanState, CodexStorageError> {
        self.require_ready()?;
        usage::load_usage_scan_state(self, source_file_ids, parser_version)
    }

    pub(crate) fn load_usage_scan_state_exact(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
        expected_epoch: crate::domain::SourceUsageEpochState,
    ) -> Result<usage::UsageScanState, CodexStorageError> {
        self.require_ready()?;
        usage::load_usage_scan_state_exact(self, source_file_ids, parser_version, expected_epoch)
    }

    pub(crate) fn commit_group(
        &self,
        batch: usage::UsageCommitBatch,
    ) -> Result<usage::UsageCommitOutcome, CodexStorageError> {
        self.require_ready()?;
        usage::commit_group(self, batch)
    }

    pub(crate) fn begin_rebuild(
        &self,
        parser_version: i64,
        present_source_ids: &[i64],
        now_ms: i64,
    ) -> Result<rebuild::BuildSnapshot, CodexStorageError> {
        self.require_ready()?;
        rebuild::begin_rebuild(self, parser_version, present_source_ids, now_ms)
    }

    pub(crate) fn replace_build_sources(
        &self,
        parser_version: i64,
        present_source_ids: &[i64],
        invalidated_source_ids: &[i64],
        now_ms: i64,
    ) -> Result<(), CodexStorageError> {
        self.require_ready()?;
        rebuild::replace_build_sources(
            self,
            parser_version,
            present_source_ids,
            invalidated_source_ids,
            now_ms,
        )
    }

    pub(crate) fn begin_carry(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> Result<(), CodexStorageError> {
        self.require_ready()?;
        usage::begin_carry(self, source_file_id, now_ms)
    }

    pub(crate) fn resume_carry(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> Result<usage::CarryStepOutcome, CodexStorageError> {
        self.require_ready()?;
        usage::resume_carry(self, source_file_id, now_ms)
    }

    pub(crate) fn complete_only(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> Result<(), CodexStorageError> {
        self.require_ready()?;
        usage::complete_only(self, source_file_id, now_ms)
    }

    pub(crate) fn quarantine_thread(
        &self,
        thread_id: &str,
        error_code: &str,
        now_ms: i64,
    ) -> Result<usize, CodexStorageError> {
        self.require_ready()?;
        rebuild::quarantine_thread(self, thread_id, error_code, now_ms)
    }

    pub(crate) fn active_quarantine_state(
        &self,
    ) -> Result<rebuild::ActiveQuarantineState, CodexStorageError> {
        self.require_ready()?;
        rebuild::active_quarantine_state(self)
    }

    pub(crate) fn activate_rebuild(
        &self,
        expected_build_epoch: i64,
        complete_present_source_ids: &[i64],
    ) -> Result<rebuild::ActivationOutcome, CodexStorageError> {
        self.require_ready()?;
        rebuild::activate_rebuild(self, expected_build_epoch, complete_present_source_ids)
    }

    pub(crate) fn cleanup_inactive(&self, max_rows: usize) -> Result<usize, CodexStorageError> {
        self.require_ready()?;
        usage::cleanup_inactive(self, max_rows)
    }
}

pub(crate) struct CodexWriteTxn<'a> {
    inner: SourceWriteTxn<'a>,
}

impl CodexWriteTxn<'_> {
    pub(crate) fn bind_or_validate(
        &mut self,
        expected_fingerprint: &str,
    ) -> Result<CodexBindingOutcome, CodexStorageError> {
        let row: (Option<String>, String) = self.with_private_state(|connection| {
            connection
                .query_row(
                    "SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(CodexStorageError::from)
        })?;
        match (row.0.as_deref(), row.1.as_str()) {
            (None, "unbound") => {
                let changed = self.with_private_state(|connection| {
                    connection
                        .execute(
                            "UPDATE codex_adapter_state SET home_fingerprint=?1,binding_status='ready' WHERE id=1",
                            [expected_fingerprint],
                        )
                        .map_err(CodexStorageError::from)
                })?;
                if changed != 1 {
                    return Err(CodexStorageError::InvalidBindingState);
                }
                self.bump_status_revision()?;
                Ok(CodexBindingOutcome::BoundNow)
            }
            (Some(stored), "ready") if stored == expected_fingerprint => {
                Ok(CodexBindingOutcome::Ready)
            }
            (Some(_), "ready") => {
                self.with_private_state(|connection| {
                    connection
                        .execute(
                            "UPDATE codex_adapter_state SET binding_status='source_changed' WHERE id=1",
                            [],
                        )
                        .map_err(CodexStorageError::from)
                })?;
                self.bump_status_revision()?;
                Ok(CodexBindingOutcome::SourceChanged)
            }
            (Some(_), "source_changed") => Ok(CodexBindingOutcome::SourceChanged),
            _ => Err(CodexStorageError::InvalidBindingState),
        }
    }

    pub(crate) fn ensure_usage_epoch(&mut self) -> Result<(), CodexStorageError> {
        self.inner.ensure_usage_epoch().map_err(Into::into)
    }

    pub(crate) fn usage_epoch_state(
        &mut self,
    ) -> Result<crate::domain::SourceUsageEpochState, CodexStorageError> {
        self.inner.usage_epoch_state().map_err(Into::into)
    }

    pub(crate) fn resolve_usage_write_epoch(
        &mut self,
        target: crate::source::UsageWriteTarget,
    ) -> Result<i64, CodexStorageError> {
        self.inner
            .resolve_usage_write_epoch(target)
            .map_err(Into::into)
    }

    pub(crate) fn begin_or_resume_usage_build(
        &mut self,
        parser_version: i64,
    ) -> Result<i64, CodexStorageError> {
        self.inner
            .begin_or_resume_usage_build(parser_version)
            .map_err(Into::into)
    }

    pub(crate) fn retarget_usage_build(
        &mut self,
        expected_build_epoch: i64,
        expected_old_parser_version: i64,
        new_parser_version: i64,
    ) -> Result<(), CodexStorageError> {
        self.inner
            .retarget_usage_build(
                expected_build_epoch,
                expected_old_parser_version,
                new_parser_version,
            )
            .map_err(Into::into)
    }

    pub(crate) fn write_usage_no_revision(
        &mut self,
        target: crate::source::UsageWriteTarget,
        event: CanonicalUsageEventWrite,
    ) -> Result<crate::source::CanonicalWriteOutcome, CodexStorageError> {
        self.inner
            .write_usage_no_revision(target, event)
            .map_err(Into::into)
    }

    pub(crate) fn copy_usage_event_no_revision(
        &mut self,
        from: crate::source::UsageWriteTarget,
        to: crate::source::UsageWriteTarget,
        event_id: &str,
    ) -> Result<crate::source::CanonicalWriteOutcome, CodexStorageError> {
        self.inner
            .copy_usage_event_no_revision(from, to, event_id)
            .map_err(Into::into)
    }

    pub(crate) fn delete_usage_events_no_revision(
        &mut self,
        target: crate::source::UsageWriteTarget,
        event_ids: &[String],
    ) -> Result<usize, CodexStorageError> {
        self.inner
            .delete_usage_events_no_revision(target, event_ids)
            .map_err(Into::into)
    }

    pub(crate) fn delete_inactive_usage_events_no_revision(
        &mut self,
        expected_epoch: i64,
        event_ids: &[String],
    ) -> Result<usize, CodexStorageError> {
        self.inner
            .delete_inactive_usage_events_no_revision(expected_epoch, event_ids)
            .map_err(Into::into)
    }

    pub(crate) fn rebind_usage_root_no_revision(
        &mut self,
        target: crate::source::UsageWriteTarget,
        thread_id: &str,
        next_root_session_id: &str,
    ) -> Result<usize, CodexStorageError> {
        self.inner
            .rebind_usage_root_no_revision(target, thread_id, next_root_session_id)
            .map_err(Into::into)
    }

    pub(crate) fn upsert_session_metadata_no_revision(
        &mut self,
        identity: &crate::domain::SessionIdentity,
        patch: &crate::domain::ResolvedThreadPatch,
    ) -> Result<crate::source::SessionMutationOutcome, CodexStorageError> {
        self.inner
            .upsert_session_metadata_no_revision(identity, patch)
            .map_err(Into::into)
    }

    pub(crate) fn activate_usage_build_with_private_visibility<F>(
        &mut self,
        expected_build_epoch: i64,
        expected_parser_version: i64,
        callback: F,
    ) -> Result<crate::source::UsageActivationOutcome, CodexStorageError>
    where
        F: FnOnce(
            &Connection,
            &crate::source::SourceId,
            i64,
            i64,
            i64,
            i64,
        ) -> Result<bool, CodexStorageError>,
    {
        self.inner
            .activate_usage_build_with_private_visibility(
                expected_build_epoch,
                expected_parser_version,
                |connection, source, active, active_parser, build, build_parser| {
                    callback(
                        connection,
                        source,
                        active,
                        active_parser,
                        build,
                        build_parser,
                    )
                },
            )
            .map_err(Into::into)
    }

    pub(crate) fn bump_data_revision(&mut self) -> Result<i64, CodexStorageError> {
        self.inner.bump_data_revision().map_err(Into::into)
    }

    pub(crate) fn bump_status_revision(&mut self) -> Result<i64, CodexStorageError> {
        self.inner.bump_status_revision().map_err(Into::into)
    }

    pub(crate) fn with_private_state<T, E>(
        &mut self,
        operation: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SourceStorageError>,
    {
        self.inner.with_private_state(operation)
    }

    pub(crate) fn commit(self) -> Result<(), CodexStorageError> {
        self.inner.commit().map_err(Into::into)
    }
}
