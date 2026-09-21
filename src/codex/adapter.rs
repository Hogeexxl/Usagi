//! Formal Codex source adapter.
//!
//! The adapter owns configuration and source binding only.  Codex ingestion
//! receives the already source-bound storage capability and never receives a
//! `Ledger` or a raw SQLite connection.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::source::{
    AdapterAvailability, SourceAdapter, SourceAdapterError, SourceDescriptor, SourceId,
    SourceRunContext, SourceRunResult,
};

use super::storage::{CodexStorage, CodexStorageError};
use super::{config::CodexConfigResolution, ingestion::CodexIngestion};

/// Error sidecar type is implemented by the analytics lane.  Keeping the
/// marker here gives the public adapter surface a stable constructor type
/// without coupling ingestion to query code.
pub use crate::codex::analytics::CodexSessionErrorSidecar;

pub struct CodexAdapter {
    descriptor: SourceDescriptor,
    config: CodexConfigResolution,
}

impl CodexAdapter {
    pub fn new(config: CodexConfigResolution) -> Self {
        Self {
            descriptor: SourceDescriptor::new(SourceId::CODEX, "Codex"),
            config,
        }
    }
}

impl SourceAdapter for CodexAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        match &self.config {
            CodexConfigResolution::Ready(_) => Ok(AdapterAvailability::Available),
            CodexConfigResolution::Invalid(error) => Err(SourceAdapterError::with_code(
                error.code(),
                error.to_string(),
            )),
        }
    }

    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult {
        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "SCAN_CANCELLED",
                "Codex scan was cancelled before execution",
            ));
        }
        let config = match &self.config {
            CodexConfigResolution::Ready(config) => config,
            CodexConfigResolution::Invalid(error) => {
                return Err(SourceAdapterError::with_code(
                    error.code(),
                    error.to_string(),
                ));
            }
        };
        let storage = CodexStorage::new(context.storage()).map_err(adapter_error)?;
        let mut binding = storage.begin_write_txn().map_err(adapter_error)?;
        let outcome = binding
            .bind_or_validate(config.home_fingerprint())
            .map_err(adapter_error)?;
        binding.commit().map_err(adapter_error)?;
        if matches!(
            outcome,
            super::storage::binding::CodexBindingOutcome::SourceChanged
        ) {
            return Err(SourceAdapterError::with_code(
                "SOURCE_CHANGED",
                "Codex home fingerprint changed",
            ));
        }
        CodexIngestion::run(config, &storage, cancellation)
            .map_err(|code| SourceAdapterError::with_code(code, "Codex ingestion failed"))
    }
}

fn adapter_error(error: CodexStorageError) -> SourceAdapterError {
    SourceAdapterError::with_code(error.code(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_identity_keeps_native_session_id_as_thread_id() {
        let native = "00000000-0000-4000-8000-000000000001";
        let identity = crate::domain::SessionIdentity::new(native, SourceId::CODEX, native)
            .expect("Codex identity must be valid");
        assert_eq!(identity.thread_id, native);
        assert_eq!(identity.native_session_id, native);
        assert_eq!(identity.source, SourceId::CODEX);
    }
}
