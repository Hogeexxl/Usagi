use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};

use super::{
    Ledger, LedgerOptions, PragmaState, StorageErrorKind, migrate_legacy_database_if_needed,
    usage_event_count,
};
use crate::codex::storage::CodexBindingStatus;
use crate::domain::{AppState, FollowupState, ScanTrigger};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("usagi-storage-{unique}"));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(root: &TempDir) -> LedgerOptions {
    LedgerOptions::new(root.path().join("nested/db/mu.sqlite3"))
}

fn codex_binding_status(ledger: &Ledger) -> CodexBindingStatus {
    let connection = ledger.connection().unwrap();
    let value: String = connection
        .query_row(
            "SELECT binding_status FROM codex_adapter_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    CodexBindingStatus::parse(&value).unwrap()
}

fn seed_usage_database(path: &Path, rows: usize) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("CREATE TABLE usage_events (event_id TEXT PRIMARY KEY);")
        .unwrap();
    for index in 0..rows {
        connection
            .execute(
                "INSERT INTO usage_events(event_id) VALUES (?1)",
                [format!("event-{index}")],
            )
            .unwrap();
    }
}

#[test]
fn rename_migration_moves_legacy_directory_when_new_database_is_missing() {
    let root = TempDir::new();
    let legacy = root.path().join("legacy-name").join("mu.sqlite3");
    let current = root.path().join("Usagi").join("mu.sqlite3");
    seed_usage_database(&legacy, 2);
    fs::write(legacy.parent().unwrap().join("sidecar-state"), b"keep").unwrap();

    migrate_legacy_database_if_needed(&current, &legacy).unwrap();

    assert_eq!(usage_event_count(&current).unwrap(), 2);
    assert_eq!(
        fs::read(current.parent().unwrap().join("sidecar-state")).unwrap(),
        b"keep"
    );
    assert!(!legacy.parent().unwrap().exists());
}

#[test]
fn rename_migration_replaces_empty_database_created_by_broken_rename_build() {
    let root = TempDir::new();
    let legacy = root.path().join("legacy-name").join("mu.sqlite3");
    let current = root.path().join("Usagi").join("mu.sqlite3");
    seed_usage_database(&legacy, 3);
    seed_usage_database(&current, 0);

    migrate_legacy_database_if_needed(&current, &legacy).unwrap();

    assert_eq!(usage_event_count(&current).unwrap(), 3);
    assert!(!legacy.parent().unwrap().exists());
}

#[test]
fn rename_migration_never_overwrites_nonempty_usagi_database() {
    let root = TempDir::new();
    let legacy = root.path().join("legacy-name").join("mu.sqlite3");
    let current = root.path().join("Usagi").join("mu.sqlite3");
    seed_usage_database(&legacy, 3);
    seed_usage_database(&current, 1);

    migrate_legacy_database_if_needed(&current, &legacy).unwrap();

    assert_eq!(usage_event_count(&current).unwrap(), 1);
    assert_eq!(usage_event_count(&legacy).unwrap(), 3);
}

#[test]
fn opens_nested_database_and_verifies_pragmas() {
    let root = TempDir::new();
    let ledger = Ledger::open(options(&root)).unwrap();
    assert!(ledger.database_path().exists());
    assert_eq!(ledger.schema_version().unwrap(), 13);
    assert_eq!(
        ledger.pragma_state().unwrap(),
        PragmaState {
            journal_mode_wal: true,
            synchronous_normal: true,
            foreign_keys: true,
            busy_timeout_ms: 5_000,
        }
    );
    let state = ledger.app_state().unwrap();
    assert_eq!(state.data_revision, 0);
    assert_eq!(state.status_revision, 0);
    assert_eq!(state.data_revision, 0);
    assert!(!root.path().join("codex").is_dir());
}

#[test]
fn reopening_preserves_binding_and_schema() {
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    drop(first);
    let second = Ledger::open(opts).unwrap();
    assert_eq!(second.schema_version().unwrap(), 13);
    assert_eq!(second.app_state().unwrap().status_revision, 0);
}

#[test]
fn mismatched_home_is_readable_but_not_writable() {
    let root = TempDir::new();
    let db = root.path().join("mu.sqlite3");
    let first = Ledger::open(LedgerOptions::new(&db)).unwrap();
    drop(first);

    let changed = Ledger::open(LedgerOptions::new(&db)).unwrap();
    assert_eq!(codex_binding_status(&changed), CodexBindingStatus::Unbound);
    assert_eq!(changed.app_state().unwrap().status_revision, 0);
    assert_eq!(changed.schema_version().unwrap(), 13);
    drop(changed);

    // Reopening with the original source remains readable, while explicit
    // recovery is still required to clear source_changed.
    let original = Ledger::open(LedgerOptions::new(db)).unwrap();
    assert_eq!(codex_binding_status(&original), CodexBindingStatus::Unbound);
}

#[test]
fn app_state_uses_domain_projection_and_preserves_queued_followup_without_binding() {
    let root = TempDir::new();
    let db = root.path().join("mu.sqlite3");
    let first = Ledger::open(LedgerOptions::new(&db)).unwrap();

    let _: AppState = first.app_state().unwrap();
    let connection = first.connection().unwrap();
    connection
        .execute(
            "INSERT INTO scan_runs (
                    scan_id, trigger, request_kind, state, requested_at_ms,
                    enqueued_status_revision
                 ) VALUES ('followup-1', 'Manual', 'followup', 'queued', 100, 0)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE app_meta
                 SET followup_scan_id = 'followup-1',
                     followup_state = 'queued',
                     followup_trigger = 'Manual',
                     followup_requested_at_ms = 100,
                     followup_enqueued_status_revision = 0
                 WHERE id = 1",
            [],
        )
        .unwrap();
    drop(connection);
    drop(first);

    let changed = Ledger::open(LedgerOptions::new(&db)).unwrap();
    let state = changed.app_state().unwrap();
    assert_eq!(codex_binding_status(&changed), CodexBindingStatus::Unbound);
    assert_eq!(state.status_revision, 0);
    assert_eq!(state.followup_state, Some(FollowupState::Queued));
    assert_eq!(state.followup_trigger, Some(ScanTrigger::Manual));
    assert_eq!(state.followup_error_code, None);

    let connection = changed.connection().unwrap();
    let row: (String, Option<i64>, Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT state, started_at_ms, terminal_status_revision, error_code
                 FROM scan_runs WHERE scan_id = 'followup-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row.0, "queued");
    assert!(row.1.is_none());
    assert!(row.2.is_none());
    assert!(row.3.is_none());
}

#[test]
fn newer_schema_is_rejected_without_deleting_database() {
    let root = TempDir::new();
    let db = root.path().join("future.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection
        .pragma_update(None, "user_version", 99_i64)
        .unwrap();
    drop(connection);
    let before = fs::read(&db).unwrap();
    let error = Ledger::open(LedgerOptions::new(&db)).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::SchemaTooNew);
    assert_eq!(error.schema_versions(), Some((99, 13)));
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn corrupt_database_is_not_removed() {
    let root = TempDir::new();
    let db = root.path().join("corrupt.sqlite3");
    let bytes = b"not a sqlite database";
    fs::write(&db, bytes).unwrap();
    let error = Ledger::open(LedgerOptions::new(&db)).unwrap_err();
    assert!(matches!(
        error.kind(),
        StorageErrorKind::DatabaseCorrupt | StorageErrorKind::Database
    ));
    assert_eq!(fs::read(&db).unwrap(), bytes);
}

#[test]
fn migration_failure_rolls_back_schema_and_version() {
    let root = TempDir::new();
    let db = root.path().join("migration-failure.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(include_str!("migration_failure_fixture.sql"))
        .unwrap();
    drop(connection);

    assert!(Ledger::open(LedgerOptions::new(&db)).is_err());
    let connection = Connection::open(&db).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let sentinel: i64 = connection
        .query_row(include_str!("migration_failure_count.sql"), [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(sentinel, 0);
    assert!(
        connection
            .query_row::<i64, _, _>("SELECT count(*) FROM app_meta", [], |row| row.get(0))
            .is_err()
    );
}

fn seed_cost_events(ledger: &Ledger, overflow: bool) -> i64 {
    let connection = ledger.connection().unwrap();
    connection
        .execute(
            "UPDATE source_usage_epochs
                 SET active_epoch=1,active_parser_version=7
                 WHERE source='codex'",
            [],
        )
        .unwrap();
    connection
            .execute(
                "INSERT INTO threads(
                    thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,archived,
                    project_kind,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('cost-root','codex','cost-root',NULL,'cost-root','main',0,'unknown','complete',0)",
                [],
            )
            .unwrap();
    connection
            .execute(
                "INSERT INTO codex_source_files(
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (1,'cost-root','/tmp/usagi-cost.jsonl','sessions',1,1,1,100,0,'present',0)",
                [],
            )
            .unwrap();
    if overflow {
        connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,created_at_ms
                     ) VALUES ('codex',1,'auto-review','normal',0,'cost-root','cost-root',NULL,'codex-auto-review',NULL,NULL,
                               1000,200,100,50,20,1050,'complete',0),
                              ('codex',1,'overflow','normal',0,'cost-root','cost-root',NULL,'gpt-5.6-sol',NULL,NULL,
                               9000000000000000,0,0,0,0,9000000000000000,'complete',0)",
                    [],
                )
                .unwrap();
    } else {
        connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,created_at_ms
                     ) VALUES ('codex',1,'known','normal',0,'cost-root','cost-root',NULL,'gpt-5.6-sol','high',NULL,
                               1000,200,100,50,20,1050,'complete',0),
                              ('codex',1,'auto-review','normal',0,'cost-root','cost-root',NULL,'codex-auto-review','high',NULL,
                               1000,200,100,50,20,1050,'complete',0),
                              ('codex',1,'unknown','recovered',0,'cost-root','cost-root',NULL,'unknown-model',NULL,NULL,
                               1000,200,100,50,20,1050,'complete',0)",
                    [],
                )
                .unwrap();
    }
    let revision: i64 = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute(
            "UPDATE app_meta SET cost_algorithm_version=0,pricing_catalog_version=0 WHERE id=1",
            [],
        )
        .unwrap();
    revision
}

fn insert_cost_event(
    connection: &Connection,
    source: &str,
    source_epoch: i64,
    event_id: &str,
    model: &str,
    estimated_cost_nanos_usd: Option<i64>,
) {
    connection
        .execute(
            "INSERT INTO usage_events(
                    source,source_epoch,event_id,event_kind,occurred_at_ms,
                    thread_id,root_session_id,turn_key,model,reasoning_effort,
                    estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,
                    output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms
                 ) VALUES (?1,?2,?3,'normal',0,'cost-root','cost-root',NULL,?4,NULL,?5,
                           1000,200,100,50,20,1050,'complete',0)",
            params![
                source,
                source_epoch,
                event_id,
                model,
                estimated_cost_nanos_usd,
            ],
        )
        .unwrap();
}

fn mark_cost_versions_stale(connection: &Connection) {
    connection
        .execute(
            "UPDATE app_meta
                 SET cost_algorithm_version=0,pricing_catalog_version=0
                 WHERE id=1",
            [],
        )
        .unwrap();
}

#[test]
fn t_mu04_a02_open_reprices_pricing_catalog_atomically() {
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, false);
    let parser_version_before: i64 = first
        .connection()
        .unwrap()
        .query_row(
            "SELECT active_parser_version FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(first);

    let reopened = Ledger::open(opts).unwrap();
    let connection = reopened.connection().unwrap();
    type RepricedCosts = (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
    );
    let costs: RepricedCosts = connection
            .query_row(
                "SELECT
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='known'),
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='unknown'),
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='auto-review'),
                    (SELECT model FROM usage_events WHERE event_id='auto-review'),
                    (SELECT cost_algorithm_version FROM app_meta WHERE id=1),
                    (SELECT pricing_catalog_version FROM app_meta WHERE id=1),
                    (SELECT data_revision FROM app_meta WHERE id=1),
                    (SELECT active_parser_version FROM source_usage_epochs WHERE source='codex')",
                [],
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
                    ))
                },
            )
            .unwrap();
    assert_eq!(
        costs,
        (
            Some(4_380_000),
            None,
            Some(229_000),
            Some("codex-auto-review".to_owned()),
            1,
            4,
            before_revision + 1,
            parser_version_before,
        )
    );
    drop(connection);
    assert_eq!(
        reopened.current_revision().data_revision,
        before_revision + 1
    );
}

#[test]
fn t_mu03_b05_open_reprice_rolls_back_on_overflow() {
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, true);
    drop(first);

    assert!(Ledger::open(opts.clone()).is_err());
    let connection = Connection::open(root.path().join("nested/db/mu.sqlite3")).unwrap();
    let state: (Option<i64>, Option<i64>, i64, i64, i64) = connection
        .query_row(
            "SELECT
                        (SELECT estimated_cost_nanos_usd FROM usage_events
                         WHERE event_id='auto-review'),
                        (SELECT estimated_cost_nanos_usd FROM usage_events
                         WHERE event_id='overflow'),
                        cost_algorithm_version,
                        pricing_catalog_version,data_revision
                 FROM usage_events JOIN app_meta ON app_meta.id=1
                 WHERE usage_events.event_id='overflow'",
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
    assert_eq!(state, (None, None, 0, 0, before_revision));
}

#[test]
fn t_q08_cost_repricing_revision_matrix() {
    // (1) Inactive and build history is repriced, but no active row is
    // visible, so the dashboard revision remains unchanged.
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, false);
    let connection = first.connection().unwrap();
    connection
        .execute(
            "UPDATE source_usage_epochs
                 SET active_epoch=2,build_epoch=3,build_parser_version=7
                 WHERE source='codex'",
            [],
        )
        .unwrap();
    insert_cost_event(
        &connection,
        "codex",
        3,
        "build-only",
        "gpt-5.6-sol",
        Some(0),
    );
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=0
                 WHERE source='codex' AND event_id IN ('known','build-only')",
            [],
        )
        .unwrap();
    mark_cost_versions_stale(&connection);
    drop(connection);
    drop(first);

    let reopened = Ledger::open(opts).unwrap();
    let connection = reopened.connection().unwrap();
    let (inactive, build, revision): (Option<i64>, Option<i64>, i64) = connection
        .query_row(
            "SELECT
                    (SELECT estimated_cost_nanos_usd FROM usage_events
                     WHERE source='codex' AND source_epoch=1 AND event_id='known'),
                    (SELECT estimated_cost_nanos_usd FROM usage_events
                     WHERE source='codex' AND source_epoch=3 AND event_id='build-only'),
                    (SELECT data_revision FROM app_meta WHERE id=1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(inactive, Some(4_380_000));
    assert_eq!(build, Some(4_380_000));
    assert_eq!(revision, before_revision);

    // (2) An actual active-cost change bumps the global revision once.
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, false);
    let connection = first.connection().unwrap();
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=0
                 WHERE source='codex' AND event_id='known'",
            [],
        )
        .unwrap();
    mark_cost_versions_stale(&connection);
    drop(connection);
    drop(first);
    let reopened = Ledger::open(opts).unwrap();
    let connection = reopened.connection().unwrap();
    let (known, revision): (Option<i64>, i64) = connection
        .query_row(
            "SELECT
                    (SELECT estimated_cost_nanos_usd FROM usage_events
                     WHERE source='codex' AND source_epoch=1 AND event_id='known'),
                    (SELECT data_revision FROM app_meta WHERE id=1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(known, Some(4_380_000));
    assert_eq!(revision, before_revision + 1);

    // (3) Repricing an active row to its existing value does not bump.
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, false);
    let connection = first.connection().unwrap();
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=CASE event_id
                     WHEN 'known' THEN 4380000
                     WHEN 'auto-review' THEN 229000
                     ELSE NULL END
                 WHERE source='codex'",
            [],
        )
        .unwrap();
    mark_cost_versions_stale(&connection);
    drop(connection);
    drop(first);
    let reopened = Ledger::open(opts).unwrap();
    assert_eq!(reopened.current_revision().data_revision, before_revision);

    // (4) The complete source-aware key prevents same-event-id rows from
    // different sources from being cross-updated.
    let root = TempDir::new();
    let opts = options(&root);
    let first = Ledger::open(opts.clone()).unwrap();
    let before_revision = seed_cost_events(&first, false);
    let connection = first.connection().unwrap();
    connection
        .execute(
            "INSERT INTO source_usage_epochs(
                    source,active_epoch,active_parser_version
                 ) VALUES ('fake-source',8,1)",
            [],
        )
        .unwrap();
    insert_cost_event(
        &connection,
        "fake-source",
        8,
        "known",
        "codex-auto-review",
        Some(0),
    );
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=0
                 WHERE event_id='known'",
            [],
        )
        .unwrap();
    mark_cost_versions_stale(&connection);
    drop(connection);
    drop(first);
    let reopened = Ledger::open(opts).unwrap();
    let connection = reopened.connection().unwrap();
    let rows = connection
        .prepare(
            "SELECT source,estimated_cost_nanos_usd FROM usage_events
                 WHERE event_id='known' ORDER BY source",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("codex".to_owned(), Some(4_380_000)),
            ("fake-source".to_owned(), None),
        ]
    );
    assert_eq!(
        reopened.current_revision().data_revision,
        before_revision + 1
    );
}
