//! Integration tests for Track D Antigravity Source Adapter.
//!
//! Enforces:
//! - Only public surface used for execution (Section 4.8.8)
//! - Read-only SQL via `Ledger::database_path()` for verification
//! - Full rescan idempotency, rename stability, rewrite conflict quarantine,
//!   and coordinator cancellation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusqlite::OpenFlags;
use usagi::domain::ScanTrigger;
use usagi::ingestion::{IngestionConfig, IngestionCoordinator, RequestDisposition, ScanHandle};
use usagi::source::{
    AdapterAvailability, SourceAdapter, SourceAdapterError, SourceDescriptor, SourceId,
    SourceRegistry, SourceRunContext, SourceRunResult,
};
use usagi::storage::{Ledger, LedgerOptions};
use usagi::usage::{TimeRange, UsageFilter, UsageLedger};
use usagi::{AntigravityAdapter, AntigravityConfig, AntigravityConfigResolution};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn temp_paths(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("track-d-ag-test-{name}-{}", now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let db_path = base.join("test_ledger.db");
    let fixture_home = base.join("ag_home");
    (db_path, fixture_home)
}

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

fn wait_for_scan(scanner: &ScanHandle, ledger: &Ledger, trigger: ScanTrigger) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let scan_id = loop {
        match scanner.request(trigger) {
            Ok(RequestDisposition::Started { scan_id, .. }) => break scan_id,
            Ok(RequestDisposition::Coalesced {
                followup_scan_id, ..
            }) => break followup_scan_id,
            Err(usagi::ingestion::ScanRequestError::Recovering) => {
                if Instant::now() > deadline {
                    panic!("timed out waiting for coordinator recovery");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("request scan failed: {err:?}"),
        }
    };
    loop {
        let status = ledger
            .scan_status_snapshot(Some(&scan_id))
            .unwrap()
            .target_scan;
        if let Some(run) = status {
            if matches!(
                run.state,
                usagi::domain::ScanRunState::Completed
                    | usagi::domain::ScanRunState::Failed
                    | usagi::domain::ScanRunState::StartFailed
            ) {
                return scan_id;
            }
        }
        if Instant::now() > deadline {
            panic!("scan timed out waiting for completion: {scan_id}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// TD-P3-SOURCE-DESCRIPTOR-RUST-01: SourceDescriptor literal surface
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_source_descriptor_rust_01() {
    let desc = SourceDescriptor::new(SourceId::ANTIGRAVITY, "Antigravity");
    assert_eq!(desc.id, SourceId::ANTIGRAVITY);
    assert_eq!(desc.display_name, "Antigravity");
}

// ---------------------------------------------------------------------------
// TD-P3-PUBLIC-SURFACE-01: Public constructor and imports
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_public_surface_01() {
    let not_installed = AntigravityAdapter::new(AntigravityConfigResolution::NotInstalled);
    assert_eq!(
        not_installed.availability().unwrap(),
        AdapterAvailability::NotInstalled
    );

    let invalid = AntigravityAdapter::new(AntigravityConfigResolution::Invalid(
        usagi::AntigravityConfigError::not_a_directory(PathBuf::from("/dev/null")),
    ));
    assert!(invalid.availability().is_err());
}

// ---------------------------------------------------------------------------
// TD-P3-IMPORT-01: Complete standalone fixture import
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_import_01_complete_standalone_fixture() {
    let (db_path, ag_home) = temp_paths("import-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    // Verify canonical tables via read-only SQLite [INV-CANON-01], [INV-TOKEN-02]
    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();

    // 1. usage_events check
    let mut stmt = conn
        .prepare(
            "SELECT event_id, event_kind, occurred_at_ms, thread_id, root_session_id,
                turn_key, model, reasoning_effort, estimated_cost_nanos_usd,
                input_tokens, cached_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, total_tokens, quality_status, created_at_ms
         FROM usage_events WHERE source='antigravity' ORDER BY occurred_at_ms ASC",
        )
        .unwrap();

    let events: Vec<(
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<i64>,
        i64,
        i64,
        Option<i64>,
        i64,
        i64,
        i64,
        String,
        i64,
    )> = stmt
        .query_map([], |row| {
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
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert_eq!(events.len(), 3, "expected 3 events from standalone fixture");

    for ev in &events {
        assert_eq!(ev.1, "normal", "kind must be Normal [INV-CANON-01]");
        assert_eq!(ev.5, None, "turn_key must be None [INV-CANON-01]");
        assert_eq!(
            ev.8, None,
            "estimated_cost_nanos_usd must be None [INV-CANON-01]"
        );
        assert_eq!(
            ev.11, None,
            "cache_write_tokens must be None [INV-TOKEN-02]"
        );
        assert_eq!(
            ev.15, "partial",
            "quality_status must be partial [INV-QUALITY-01]"
        );
        assert_eq!(
            ev.16, ev.2,
            "created_at_ms must equal occurred_at_ms [INV-REV-03]"
        );
    }

    // Event 1 verification against manifest
    let e1 = &events[0];
    assert_eq!(
        e1.0,
        "effa6389-921a-497e-87e0-5a2962526c07:JZyoarzvF-ulqfkPk-CLkQc"
    );
    assert_eq!(e1.10, 24); // cached_tokens
    assert_eq!(e1.9, 1318 + 24); // input_tokens = uncached + cached = 1342
    assert_eq!(e1.12, 363); // output_tokens
    assert_eq!(e1.13, 63); // reasoning_tokens
    assert_eq!(e1.14, 1342 + 363); // total_tokens
    let mut model_efforts = events
        .iter()
        .map(|event| (event.6.clone(), event.7.clone()))
        .collect::<Vec<_>>();
    model_efforts.sort();
    assert_eq!(
        model_efforts,
        vec![
            ("gemini-3.8-flash".into(), Some("high".into())),
            ("gemini-3.8-flash".into(), Some("low".into())),
            ("gemini-3.8-pro".into(), Some("medium".into())),
        ]
    );

    let (active_epoch, active_parser_version): (i64, i64) = conn
        .query_row(
            "SELECT active_epoch, active_parser_version FROM source_usage_epochs WHERE source='antigravity'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active_epoch, active_parser_version), (1, 2));

    // 2. threads check
    let (thread_id, native_id, title, root_id, agent_role, quality): (String, String, Option<String>, Option<String>, String, String) = conn
        .query_row(
            "SELECT thread_id, native_session_id, title, root_session_id, agent_role, metadata_quality_status
             FROM threads WHERE source='antigravity'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();

    assert_eq!(
        thread_id,
        "antigravity:effa6389-921a-497e-87e0-5a2962526c07"
    );
    assert_eq!(native_id, "effa6389-921a-497e-87e0-5a2962526c07");
    assert_eq!(title.as_deref(), Some("Sanitized Standalone Session"));
    assert_eq!(root_id.as_deref(), Some(thread_id.as_str()));
    assert_eq!(agent_role, "main");
    assert_eq!(quality, "complete");

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-RESCAN-01: Identical rescan idempotency and data_revision stability
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_rescan_01_identical_snapshot() {
    let (db_path, ag_home) = temp_paths("rescan-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();

    // Scan 1
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rev_1: i64 = conn
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let events_1: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Scan 2 (identical)
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    let rev_2: i64 = conn
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let events_2: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(
        rev_1, rev_2,
        "data_revision must not bump on identical rescan [INV-REV-01]"
    );
    assert_eq!(events_1, events_2, "event count must remain identical");

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Version-1 migration, parent/root ordering, and effort-specific usage
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_v1_migration_parent_child_effort_usage() {
    const ROOT_ID: &str = "effa6389-921a-497e-87e0-5a2962526c07";
    const CHILD_ID: &str = "49e69e84-d3f2-4f56-8c17-4ea8ded58b31";
    let (db_path, ag_home) = temp_paths("v1-migration-parent-child");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    // Put the child path first to ensure event writes do not depend on discovery order.
    let conversations = ag_home.join("conversations");
    let original_root = conversations.join(format!("{ROOT_ID}.db"));
    let root_path = conversations.join("999-root.db");
    std::fs::rename(original_root, &root_path).unwrap();
    let child_path = conversations.join("000-child.db");
    std::fs::copy(&root_path, &child_path).unwrap();
    let child_db = rusqlite::Connection::open(&child_path).unwrap();
    child_db
        .execute("UPDATE trajectory_meta SET cascade_id=?1", [CHILD_ID])
        .unwrap();
    drop(child_db);

    let summaries = rusqlite::Connection::open(ag_home.join("conversation_summaries.db")).unwrap();
    summaries
        .execute(
            "INSERT INTO conversation_summaries(
                conversation_id,title,preview,step_count,last_modified_time,workspace_uris,
                status,source,project_id,agent_name,parent_conversation_id,nesting_depth
             ) SELECT ?1,?2,preview,step_count,last_modified_time,workspace_uris,
                      status,source,project_id,agent_name,?3,1
               FROM conversation_summaries WHERE conversation_id=?3",
            rusqlite::params![CHILD_ID, "Sanitized Child Session", ROOT_ID],
        )
        .unwrap();
    drop(summaries);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    registry
        .register(AntigravityAdapter::new(AntigravityConfig::from_home(
            &ag_home,
        )))
        .unwrap();
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    // Simulate a database last written by parser version 1. It stored the
    // conversation itself as its root and could not preserve selected efforts.
    let conn = rusqlite::Connection::open(ledger.database_path()).unwrap();
    conn.execute(
        "UPDATE source_usage_epochs SET active_parser_version=1 WHERE source='antigravity'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE usage_events SET model='gemini-3.8-flash', reasoning_effort=NULL,
             root_session_id=thread_id
         WHERE source='antigravity' AND source_epoch=1",
        [],
    )
    .unwrap();
    let revision_before_migration: i64 = conn
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();

    // Parser 2 must build every event in epoch 2 without comparing the changed
    // model, effort, or root against immutable parser-1 events in epoch 1.
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    let epoch_state: (i64, i64, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_epoch,active_parser_version,build_epoch,build_parser_version
             FROM source_usage_epochs WHERE source='antigravity'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(epoch_state, (2, 2, None, None));
    let migrated_revision: i64 = conn
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migrated_revision, revision_before_migration + 1);

    let (legacy_count, active_count): (i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT count(*) FROM usage_events WHERE source='antigravity' AND source_epoch=1),
                (SELECT count(*) FROM usage_events WHERE source='antigravity' AND source_epoch=2)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((legacy_count, active_count), (6, 6));

    let root_thread = format!("antigravity:{ROOT_ID}");
    let child_thread = format!("antigravity:{CHILD_ID}");
    let events_with_wrong_root: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events
             WHERE source='antigravity' AND source_epoch=2 AND root_session_id<>?1",
            [&root_thread],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events_with_wrong_root, 0);

    let mut effort_totals = conn
        .prepare(
            "SELECT model,reasoning_effort,SUM(total_tokens),COUNT(*)
             FROM usage_events WHERE source='antigravity' AND source_epoch=2
             GROUP BY model,reasoning_effort ORDER BY model,reasoning_effort",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    effort_totals.sort();
    assert_eq!(
        effort_totals,
        vec![
            ("gemini-3.8-flash".into(), Some("high".into()), 5_200, 2),
            ("gemini-3.8-flash".into(), Some("low".into()), 3_410, 2),
            ("gemini-3.8-pro".into(), Some("medium".into()), 3_900, 2),
        ]
    );

    let range = TimeRange::new(0, i64::MAX).unwrap();
    let detail = UsageLedger::new(&ledger, &[])
        .session_detail_snapshot(range, UsageFilter::default(), None, root_thread.clone())
        .unwrap()
        .value;
    assert_eq!(detail.main.subagent_count, 1);
    assert_eq!(detail.subagents.len(), 1);
    assert_eq!(detail.subagents[0].thread_id, child_thread);
    assert_eq!(
        detail.subagents[0].parent_thread_id.as_deref(),
        Some(root_thread.as_str())
    );
    assert_eq!(
        detail.subagents[0].title.as_deref(),
        Some("Sanitized Child Session")
    );
    assert_eq!(detail.main.model_usage.len(), 3);
    assert_eq!(detail.subagents[0].model_usage.len(), 3);

    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();
    let repeated_revision: i64 = conn
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(repeated_revision, migrated_revision);
    let mutation_conflicts: i64 = conn
        .query_row(
            "SELECT count(*) FROM antigravity_usage_quarantine
             WHERE reason_code='USAGE_EVENT_MUTATION_CONFLICT'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mutation_conflicts, 0);

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-RENAME-01: Physical DB rename stability
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_rename_01_physical_db_rename() {
    let (db_path, ag_home) = temp_paths("rename-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    let old_file = ag_home.join("conversations/effa6389-921a-497e-87e0-5a2962526c07.db");
    let new_file = ag_home.join("conversations/renamed_effa.db");
    std::fs::rename(&old_file, &new_file).unwrap();

    // Scan 2 after rename
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let thread_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM threads WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let event_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(
        thread_count, 1,
        "rename must not create duplicate thread [INV-ID-02]"
    );
    assert_eq!(
        event_count, 3,
        "rename must not create duplicate events [INV-ID-02]"
    );

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-IDX-REORDER-01: Index reorder does not duplicate or skip
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_idx_reorder_01() {
    let (db_path, ag_home) = temp_paths("idx-reorder-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    // Reorder indices in the conversation DB
    let conv_db_path = ag_home.join("conversations/effa6389-921a-497e-87e0-5a2962526c07.db");
    {
        let db = rusqlite::Connection::open(&conv_db_path).unwrap();
        db.execute("UPDATE gen_metadata SET idx = idx + 100", [])
            .unwrap();
        db.execute("UPDATE steps SET idx = idx + 100", []).unwrap();
    }

    // Scan 2 after reorder
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let event_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        event_count, 3,
        "idx reorder must not duplicate or skip events [INV-SCAN-01]"
    );

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-REWRITE-01: Same responseId changed comparator field -> conflict quarantine
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_rewrite_01_mutation_conflict() {
    let (db_path, ag_home) = temp_paths("rewrite-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    // Check active epoch established
    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (active_epoch, build_epoch): (i64, Option<i64>) = conn
        .query_row(
            "SELECT active_epoch, build_epoch FROM source_usage_epochs WHERE source='antigravity'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(active_epoch, 1);
    assert_eq!(build_epoch, None);

    // Modify a candidate's payload in the conversation DB to trigger compare conflict
    let conv_db_path = ag_home.join("conversations/effa6389-921a-497e-87e0-5a2962526c07.db");
    {
        let db = rusqlite::Connection::open(&conv_db_path).unwrap();
        // Modify step timestamp for responseId 'JZyoarzvF-ulqfkPk-CLkQc'
        // In steps table, find idx for step_type=15 and alter metadata timestamp seconds
        let mut proto_bytes: Vec<u8> = db
            .query_row(
                "SELECT metadata FROM steps WHERE step_type=15 ORDER BY idx ASC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Tag 1 (timestamp) -> tag 1 (seconds). Change first byte of seconds varint (offset 3).
        proto_bytes[3] ^= 0x01;
        db.execute("UPDATE steps SET metadata = ?1 WHERE idx = (SELECT idx FROM steps WHERE step_type=15 ORDER BY idx ASC LIMIT 1)", [&proto_bytes]).unwrap();
    }

    // Run scan 2
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    // Verify conflict quarantine produced [INV-REWRITE-01]
    let conflicts: i64 = conn
        .query_row(
            "SELECT count(*) FROM antigravity_usage_quarantine WHERE reason_code='USAGE_EVENT_MUTATION_CONFLICT'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        conflicts >= 1,
        "expected USAGE_EVENT_MUTATION_CONFLICT quarantine record"
    );

    // Verify build_epoch is still None (no rebuild started) [INV-EPOCH-01]
    let build_epoch_after: Option<i64> = conn
        .query_row(
            "SELECT build_epoch FROM source_usage_epochs WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(build_epoch_after, None);

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-HISTORY-01: Conversation deleted -> historical data preserved
// ---------------------------------------------------------------------------
#[test]
fn test_td_p3_history_01_conversation_deleted_preserved() {
    let (db_path, ag_home) = temp_paths("history-01");
    let standalone_src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/standalone");
    materialize_fixture_tree(&standalone_src, &ag_home);

    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    let config = AntigravityConfig::from_home(&ag_home);
    registry.register(AntigravityAdapter::new(config)).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);

    // Delete conversation DB
    let conv_db_path = ag_home.join("conversations/effa6389-921a-497e-87e0-5a2962526c07.db");
    std::fs::remove_file(&conv_db_path).unwrap();

    // Scan 2
    wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    let conn = rusqlite::Connection::open_with_flags(
        ledger.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let threads_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM threads WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let events_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(
        threads_count, 1,
        "historical thread must be preserved [INV-SCAN-02]"
    );
    assert_eq!(
        events_count, 3,
        "historical usage events must be preserved [INV-SCAN-02]"
    );

    let _ = std::fs::remove_dir_all(ag_home.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-CANCEL-PUBLIC-01: Coordinator cancellation probe
// ---------------------------------------------------------------------------
struct CancellationProbeAdapter {
    descriptor: SourceDescriptor,
    entered_tx: mpsc::SyncSender<()>,
}

impl SourceAdapter for CancellationProbeAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        Ok(AdapterAvailability::Available)
    }

    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult {
        let _txn = context.storage().begin_write_txn().unwrap();
        self.entered_tx.send(()).unwrap();
        while !cancellation.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        Err(SourceAdapterError::with_code(
            "OPERATION_CANCELLED",
            "probe cancelled",
        ))
    }
}

#[test]
fn test_td_p3_cancel_public_01() {
    let (db_path, _) = temp_paths("cancel-pub-01");
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();

    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let probe = CancellationProbeAdapter {
        descriptor: SourceDescriptor::new(SourceId::ANTIGRAVITY, "Antigravity"),
        entered_tx,
    };
    registry.register(probe).unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();

    // Wait until probe has entered write txn
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("probe should enter txn");

    // Trigger shutdown while txn is held
    let shutdown_start = Instant::now();
    scanner.shutdown().unwrap();
    assert!(
        shutdown_start.elapsed() < Duration::from_secs(5),
        "shutdown must complete promptly after cancelling worker [INV-CANCEL-02]"
    );

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// TD-P3-SOURCE-FAIL-01: Source fail isolation
// ---------------------------------------------------------------------------
struct MockHealthyAdapter {
    descriptor: SourceDescriptor,
}

impl SourceAdapter for MockHealthyAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        Ok(AdapterAvailability::Available)
    }

    fn run_scan(&self, _context: &SourceRunContext, _cancellation: &AtomicBool) -> SourceRunResult {
        Ok(())
    }
}

#[test]
fn test_td_p3_source_fail_01() {
    let (db_path, _) = temp_paths("source-fail-01");
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();

    // Healthy source
    registry
        .register(MockHealthyAdapter {
            descriptor: SourceDescriptor::new(SourceId::CODEX, "Codex"),
        })
        .unwrap();

    // Failing source: home is not a directory
    let invalid_home = db_path.clone(); // is a file!
    registry
        .register(AntigravityAdapter::new(
            AntigravityConfigResolution::Invalid(usagi::AntigravityConfigError::not_a_directory(
                invalid_home,
            )),
        ))
        .unwrap();

    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    let scan_id = wait_for_scan(&scanner, &ledger, ScanTrigger::Manual);
    scanner.shutdown().unwrap();

    // Global scan must be failed
    let status = ledger
        .scan_status_snapshot(Some(&scan_id))
        .unwrap()
        .target_scan
        .unwrap();
    assert_eq!(status.state, usagi::domain::ScanRunState::Failed);
    assert_eq!(status.error_code.as_deref(), Some("SOURCE_RUN_FAILED"));

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
}
