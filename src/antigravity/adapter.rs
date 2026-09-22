//! Antigravity Source Adapter implementation.
//!
//! Exposes public `AntigravityAdapter` and implements `SourceAdapter`.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::antigravity::ANTIGRAVITY_USAGE_PARSER_VERSION;
use crate::antigravity::config::AntigravityConfigResolution;
use crate::antigravity::normalization::{
    AntigravityQuarantineRecord, USAGE_EVENT_MUTATION_CONFLICT,
};
use crate::antigravity::snapshot::take_source_snapshot;
use crate::antigravity::storage::{
    UsageEpochPlan, replace_conversation_quarantine, upsert_conversation_state,
    validate_usage_epoch,
};
use crate::source::adapter::{
    AdapterAvailability, CanonicalUsageEventMatch, CanonicalWriteOutcome, SourceAdapter,
    SourceAdapterError, SourceRunContext, SourceRunResult, UsageWriteTarget,
};
use crate::source::{SourceDescriptor, SourceId};

#[cfg(test)]
pub(crate) static TEST_TXN_BARRIER: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_TXN_ENTERED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
fn check_test_barrier(cancellation: &AtomicBool) -> SourceRunResult {
    if TEST_TXN_BARRIER.load(Ordering::SeqCst) {
        TEST_TXN_ENTERED.store(true, Ordering::SeqCst);
        while !cancellation.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        return Err(SourceAdapterError::with_code(
            "OPERATION_CANCELLED",
            "test barrier cancelled",
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn check_test_barrier(_cancellation: &AtomicBool) -> SourceRunResult {
    Ok(())
}

/// The Antigravity Source Adapter for Usagi.
pub struct AntigravityAdapter {
    descriptor: SourceDescriptor,
    config: AntigravityConfigResolution,
}

impl AntigravityAdapter {
    pub fn new(config: AntigravityConfigResolution) -> Self {
        Self {
            descriptor: SourceDescriptor::new(SourceId::ANTIGRAVITY, "Antigravity"),
            config,
        }
    }
}

impl SourceAdapter for AntigravityAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        match &self.config {
            AntigravityConfigResolution::Ready(_) => Ok(AdapterAvailability::Available),
            AntigravityConfigResolution::NotInstalled => Ok(AdapterAvailability::NotInstalled),
            AntigravityConfigResolution::Invalid(err) => {
                Err(SourceAdapterError::with_code(err.code(), err.to_string()))
            }
        }
    }

    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult {
        // [INV-CANCEL-01] Check before execution
        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "OPERATION_CANCELLED",
                "Antigravity scan was cancelled before execution",
            ));
        }

        let config = match &self.config {
            AntigravityConfigResolution::Ready(cfg) => cfg,
            AntigravityConfigResolution::NotInstalled => return Ok(()),
            AntigravityConfigResolution::Invalid(err) => {
                return Err(SourceAdapterError::with_code(err.code(), err.to_string()));
            }
        };

        // 1. Transaction-free source snapshot [INV-WAL-03], [INV-EXT-04], [INV-TXN-02]
        let snapshot = take_source_snapshot(config, cancellation)?;

        // [INV-CANCEL-01] Check before opening Usagi transaction
        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "OPERATION_CANCELLED",
                "Antigravity scan was cancelled before opening transaction",
            ));
        }

        // 2. Open Usagi write transaction [INV-TXN-01]
        let mut txn = context.storage().begin_write_txn().map_err(|e| {
            SourceAdapterError::with_code("ANTIGRAVITY_STORAGE_ERROR", e.to_string())
        })?;

        // Test barrier check
        check_test_barrier(cancellation)?;

        // 3. Usage epoch target resolution [INV-EPOCH-01], Section 4.10.2
        let epoch_plan = txn
            .with_private_state(|conn| validate_usage_epoch(conn))
            .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;

        let (target, is_initial_build) = match epoch_plan {
            UsageEpochPlan::InitialCreate {
                next_build_epoch: _,
            } => {
                txn.ensure_usage_epoch().map_err(|e| {
                    SourceAdapterError::with_code("ANTIGRAVITY_STORAGE_ERROR", e.to_string())
                })?;
                let _build = txn
                    .begin_or_resume_usage_build(ANTIGRAVITY_USAGE_PARSER_VERSION)
                    .map_err(|e| {
                        SourceAdapterError::with_code("ANTIGRAVITY_STORAGE_ERROR", e.to_string())
                    })?;
                (UsageWriteTarget::Build, true)
            }
            UsageEpochPlan::Active { epoch: _ } => (UsageWriteTarget::Active, false),
        };

        let mut metadata_visible_changed = false;
        let mut active_usage_inserted = false;
        let now_ms = now_ms();

        // 4. In-transaction mutation sequence per [INV-TXN-02]
        for conv in snapshot.conversations {
            // [INV-CANCEL-01] Check before conversation mutation
            if cancellation.load(Ordering::Acquire) {
                return Err(SourceAdapterError::with_code(
                    "OPERATION_CANCELLED",
                    "Antigravity scan was cancelled during conversation mutation",
                ));
            }

            // A. Conversation metadata upsert
            if let Some(patch) = &conv.metadata_patch {
                let outcome = txn
                    .upsert_session_metadata_no_revision(&conv.identity, patch)
                    .map_err(|e| {
                        SourceAdapterError::with_code("CANONICAL_METADATA_ERROR", e.to_string())
                    })?;
                if outcome.visible_changed {
                    metadata_visible_changed = true;
                }
            }

            // B. Canonical event compare & mutation-conflict quarantine
            let mut mutation_conflicts = Vec::new();
            for (rec, event) in conv.valid_records {
                if cancellation.load(Ordering::Acquire) {
                    return Err(SourceAdapterError::with_code(
                        "OPERATION_CANCELLED",
                        "Antigravity scan was cancelled during event mutation",
                    ));
                }

                let cmp = txn
                    .compare_usage_event_no_revision(target, &event)
                    .map_err(|e| {
                        SourceAdapterError::with_code("CANONICAL_COMPARE_ERROR", e.to_string())
                    })?;

                match cmp {
                    CanonicalUsageEventMatch::Conflict => {
                        mutation_conflicts.push(AntigravityQuarantineRecord {
                            conversation_id: conv.conversation_id.clone(),
                            payload_digest: rec.payload_digest,
                            gen_idx: Some(rec.gen_idx),
                            response_id: Some(rec.response_id),
                            reason_code: USAGE_EVENT_MUTATION_CONFLICT,
                        });
                    }
                    CanonicalUsageEventMatch::Absent => {
                        let outcome = txn.write_usage_no_revision(target, event).map_err(|e| {
                            SourceAdapterError::with_code("CANONICAL_WRITE_ERROR", e.to_string())
                        })?;
                        if target == UsageWriteTarget::Active
                            && outcome == CanonicalWriteOutcome::Inserted
                        {
                            active_usage_inserted = true;
                        }
                    }
                    CanonicalUsageEventMatch::Identical => {
                        // no-op per [INV-REWRITE-01]
                    }
                }
            }

            // C. Conversation observation upsert (parent must precede child quarantine for FK)
            txn.with_private_state(|conn| {
                upsert_conversation_state(
                    conn,
                    &conv.conversation_id,
                    conv.observed_gen_max_idx,
                    conv.observed_step_max_idx,
                    now_ms,
                )
            })
            .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;

            // D. Conversation quarantine replace
            let mut all_quarantines = conv.source_quarantines;
            all_quarantines.extend(mutation_conflicts);
            txn.with_private_state(|conn| {
                replace_conversation_quarantine(
                    conn,
                    &conv.conversation_id,
                    &all_quarantines,
                    now_ms,
                )
            })
            .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;
        }

        // 5. Transient initial build activation (initial state only) [INV-TXN-02], [INV-REV-02]
        if is_initial_build {
            if cancellation.load(Ordering::Acquire) {
                return Err(SourceAdapterError::with_code(
                    "OPERATION_CANCELLED",
                    "Antigravity scan was cancelled before build activation",
                ));
            }
            txn.activate_usage_build(1, ANTIGRAVITY_USAGE_PARSER_VERSION)
                .map_err(|e| {
                    SourceAdapterError::with_code("BUILD_ACTIVATION_FAILED", e.to_string())
                })?;
        }

        // 6. Revision closing per [INV-REV-02]
        if metadata_visible_changed || active_usage_inserted {
            txn.bump_data_revision().map_err(|e| {
                SourceAdapterError::with_code("BUMP_REVISION_FAILED", e.to_string())
            })?;
        }

        // 7. Final cancellation check before commit [INV-CANCEL-01], [INV-TXN-02]
        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "OPERATION_CANCELLED",
                "Antigravity scan was cancelled before commit",
            ));
        }

        // 8. Commit
        txn.commit()
            .map_err(|e| SourceAdapterError::with_code("COMMIT_FAILED", e.to_string()))?;

        Ok(())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::antigravity::config::AntigravityConfig;
    use crate::source::adapter::SourceStorageFactory;
    use crate::storage::{Ledger, LedgerOptions};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn materialize_fixture_tree(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let ty = entry.file_type().unwrap();
            let source_name = entry.file_name();
            let source_name = source_name
                .to_str()
                .expect("fixture filenames must be valid UTF-8");
            if source_name.ends_with(".db") {
                panic!(
                    "fixture database payloads must use .db.fixture: {}",
                    entry.path().display()
                );
            }
            let runtime_name = source_name.strip_suffix(".fixture").unwrap_or(source_name);
            let target = dst.join(runtime_name);
            if ty.is_dir() {
                materialize_fixture_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn materialize_standalone_fixture(dst: &Path) {
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
        materialize_fixture_tree(&source, dst);
    }

    #[test]
    fn test_adapter_availability() {
        let not_installed = AntigravityAdapter::new(AntigravityConfigResolution::NotInstalled);
        assert_eq!(
            not_installed.availability().unwrap(),
            AdapterAvailability::NotInstalled
        );

        let fixture_root = std::env::temp_dir().join(format!("usagi-ag-availability-{}", now_ms()));
        let fixture_home = fixture_root.join("standalone");
        materialize_standalone_fixture(&fixture_home);
        let ready = AntigravityAdapter::new(AntigravityConfig::from_home(&fixture_home));
        assert_eq!(
            ready.availability().unwrap(),
            AdapterAvailability::Available
        );
        let _ = std::fs::remove_dir_all(fixture_root);
    }

    #[test]
    fn test_td_p3_cancel_unit_01_transaction_rollback() {
        let temp_dir = std::env::temp_dir().join(format!("usagi-ag-cancel-test-{}", now_ms()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("ledger.db");
        let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());

        let fixture_home = temp_dir.join("standalone");
        materialize_standalone_fixture(&fixture_home);
        let config = AntigravityConfig::from_home(&fixture_home);

        let adapter = Arc::new(AntigravityAdapter::new(config));
        let factory = SourceStorageFactory::new(Arc::clone(&ledger));
        let context = factory
            .context("scan-test-1", adapter.descriptor())
            .unwrap();

        let cancellation = Arc::new(AtomicBool::new(false));

        // Enable test barrier
        TEST_TXN_BARRIER.store(true, Ordering::SeqCst);
        TEST_TXN_ENTERED.store(false, Ordering::SeqCst);

        let cancellation_clone = Arc::clone(&cancellation);
        let adapter_clone = Arc::clone(&adapter);

        let handle =
            std::thread::spawn(move || adapter_clone.run_scan(&context, &cancellation_clone));

        // Wait for thread to enter transaction
        while !TEST_TXN_ENTERED.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }

        // Trigger cancellation while inside transaction
        cancellation.store(true, Ordering::Release);

        let res = handle.join().unwrap();
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().code(), "OPERATION_CANCELLED");

        // Reset barrier
        TEST_TXN_BARRIER.store(false, Ordering::SeqCst);

        // Verify rollback: 0 usage rows, 0 threads, 0 private state
        let conn = ledger.connection().unwrap();
        let usage_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM usage_events WHERE source='antigravity'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(usage_count, 0);

        let thread_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM threads WHERE source='antigravity'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(thread_count, 0);

        let state_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM antigravity_conversation_state",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state_count, 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
