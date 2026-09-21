use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};

use crate::{
    codex::config::{CodexConfig, CodexConfigResolution},
    source::{SourceId, SourceStorage},
    storage::{Ledger, LedgerOptions},
};

use super::{CodexBindingOutcome, CodexBindingStatus};

#[test]
fn migration_binding_state_preserves_fingerprint_compatibility() {
    assert_eq!(
        CodexBindingStatus::parse("unbound"),
        Some(CodexBindingStatus::Unbound)
    );
    assert_eq!(
        CodexBindingStatus::parse("ready"),
        Some(CodexBindingStatus::Ready)
    );
    assert_eq!(
        CodexBindingStatus::parse("source_changed"),
        Some(CodexBindingStatus::SourceChanged)
    );
    assert_eq!(CodexBindingStatus::parse("unknown"), None);

    let temp_root = std::env::temp_dir().join(format!(
        "usagi-binding-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_root).expect("temporary database directory");
    let home = temp_root.join("codex-home");
    fs::create_dir_all(&home).expect("create codex home");
    let config = match CodexConfig::from_home(&home) {
        CodexConfigResolution::Ready(config) => config,
        CodexConfigResolution::Invalid(error) => panic!("resolve codex home: {error}"),
    };
    let fingerprint = config.home_fingerprint().to_owned();

    let db = temp_root.join("mu.sqlite3");
    let mut connection = Connection::open(&db).expect("create v11 database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys while installing v11 fixture");
    for schema in [
        include_str!("../../../storage/schema/0001_initial.sql"),
        include_str!("../../../storage/schema/0002_usage_ledger.sql"),
        include_str!("../../../storage/schema/0003_normalized_token_usage.sql"),
        include_str!("../../../storage/schema/0004_metadata_parent_v2_cleanup.sql"),
        include_str!("../../../storage/schema/0005_project_kind.sql"),
        include_str!("../../../storage/schema/0006_subagent_agent_path.sql"),
        include_str!("../../../storage/schema/0007_usage_context_and_estimated_cost.sql"),
        include_str!("../../../storage/schema/0008_session_resilience.sql"),
        include_str!("../../../storage/schema/0009_skill_usage_events.sql"),
        include_str!("../../../storage/schema/0010_metadata_fact_ordering.sql"),
        include_str!("../../../storage/schema/0011_multi_source_core.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("install v11 schema");
    }
    connection
        .pragma_update(None, "user_version", 11_i64)
        .expect("mark v11 fixture");
    connection
        .execute(
            "UPDATE app_meta SET
                metadata_parser_version=3,
                data_revision=19,
                status_revision=37,
                scan_state='failed',
                last_finished_scan_id='finished-scan',
                last_finished_scan_result='failed',
                last_scan_started_at_ms=1,
                last_scan_completed_at_ms=2,
                last_scan_failed_at_ms=3,
                last_scan_error_code='SCAN_ERROR',
                followup_scan_id='followup-scan',
                followup_state='start_failed',
                followup_trigger='Manual',
                followup_requested_at_ms=4,
                followup_enqueued_status_revision=37,
                followup_error_code='FOLLOWUP_ERROR',
                last_full_import_completed_at_ms=5,
                codex_home_fingerprint=?1,
                source_binding_status='ready',
                cost_algorithm_version=1,
                pricing_catalog_version=4
             WHERE id=1",
            [fingerprint.as_str()],
        )
        .expect("seed app metadata");
    crate::storage::seed_v11_binding_rows(&connection);
    let canonical_counts: Vec<(&str, i64)> = ["threads", "usage_events"]
        .into_iter()
        .map(|table| {
            (
                table,
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("count v11 canonical row"),
            )
        })
        .collect();
    let pre_counts: Vec<(String, i64)> = [
        "usage_event_occurrences",
        "source_files",
        "source_checkpoints",
        "rollout_metadata_facts",
        "turns",
        "ingest_anomalies",
        "usage_source_states",
        "usage_build_sources",
        "usage_session_quarantine",
        "usage_session_quarantine_sources",
        "skill_usage_events",
    ]
    .into_iter()
    .map(|table| {
        let count = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count v11 fixture row");
        (table.to_owned(), count)
    })
    .collect();
    drop(connection);

    let ledger = Arc::new(
        Ledger::open(LedgerOptions::for_database(&db)).expect("migrate v11 fixture to v12"),
    );
    assert_eq!(ledger.schema_version().expect("read schema version"), 12);
    let migrated = Connection::open(&db).expect("reopen migrated database");
    let version: i64 = migrated
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated user version");
    assert_eq!(version, 12);
    for (table, count) in &canonical_counts {
        assert_eq!(
            migrated
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count migrated canonical rows"),
            *count,
            "migration changed {table} row count"
        );
    }
    for (old, count) in &pre_counts {
        let new = format!("codex_{old}");
        assert_eq!(
            migrated
                .query_row(&format!("SELECT count(*) FROM {new}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count migrated private rows"),
            *count,
            "migration changed {old} row count"
        );
        assert_eq!(
            migrated
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [old],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check legacy table removal"),
            0,
            "legacy private table remains: {old}"
        );
    }
    let binding: (Option<String>, String) = migrated
        .query_row(
            "SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read migrated binding");
    assert_eq!(binding, (Some(fingerprint.clone()), "ready".to_owned()));
    let app_meta: (i64, i64, i64, i64, String, String, String, i64) = migrated
        .query_row(
            "SELECT data_revision,status_revision,cost_algorithm_version,pricing_catalog_version,
                    scan_state,last_finished_scan_id,followup_state,followup_enqueued_status_revision
             FROM app_meta WHERE id=1",
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
        .expect("read migrated app metadata");
    assert_eq!(
        app_meta,
        (
            19,
            37,
            1,
            4,
            "failed".to_owned(),
            "finished-scan".to_owned(),
            "start_failed".to_owned(),
            37
        )
    );
    for removed in [
        "metadata_parser_version",
        "last_full_import_completed_at_ms",
        "codex_home_fingerprint",
        "source_binding_status",
    ] {
        assert_eq!(
            migrated
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('app_meta') WHERE name=?1",
                    [removed],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check removed app_meta column"),
            0,
            "removed app_meta column remains: {removed}"
        );
    }
    for (kind, name) in [
        ("trigger", "codex_source_checkpoints_offset_insert"),
        ("trigger", "codex_source_checkpoints_offset_update"),
        ("index", "codex_source_files_thread_idx"),
        ("index", "codex_usage_event_occurrences_event_idx"),
    ] {
        assert_eq!(
            migrated
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type=?1 AND name=?2",
                    params![kind, name],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check migrated schema object"),
            1,
            "missing migrated {kind} {name}"
        );
    }
    for (kind, name) in [
        ("trigger", "source_checkpoints_offset_insert"),
        ("trigger", "source_checkpoints_offset_update"),
        ("index", "source_files_thread_idx"),
        ("index", "usage_event_occurrences_event_idx"),
    ] {
        assert_eq!(
            migrated
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type=?1 AND name=?2",
                    params![kind, name],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check legacy schema object removal"),
            0,
            "legacy {kind} remains: {name}"
        );
    }
    let mut foreign_keys = migrated
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare foreign key check");
    assert!(
        foreign_keys
            .query([])
            .expect("run foreign key check")
            .next()
            .expect("read foreign key check")
            .is_none()
    );
    drop(foreign_keys);
    let occurrence_primary_key: i64 = migrated
        .query_row(
            "SELECT count(*) FROM pragma_table_info('codex_usage_event_occurrences')
             WHERE pk IN (1,2,3,4,5)",
            [],
            |row| row.get(0),
        )
        .expect("read occurrence primary key");
    assert_eq!(occurrence_primary_key, 5);
    drop(migrated);

    let storage = SourceStorage::with_ledger("binding-test", SourceId::CODEX, Arc::clone(&ledger));
    let codex = crate::codex::storage::CodexStorage::new(&storage).expect("create Codex storage");
    let mut binding_tx = codex
        .begin_write_txn()
        .expect("begin compatibility binding");
    assert_eq!(
        binding_tx
            .bind_or_validate(config.home_fingerprint())
            .expect("validate migrated fingerprint"),
        CodexBindingOutcome::Ready
    );
    binding_tx.commit().expect("commit compatible binding");
    let reopened = Connection::open(&db).expect("reopen after binding validation");
    let (status_revision, stored_fingerprint): (i64, String) = reopened
        .query_row(
            "SELECT app_meta.status_revision,codex_adapter_state.home_fingerprint
             FROM app_meta CROSS JOIN codex_adapter_state
             WHERE app_meta.id=1 AND codex_adapter_state.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read post-binding revisions");
    assert_eq!(status_revision, 37);
    assert_eq!(stored_fingerprint, fingerprint);
    drop(codex);
    drop(storage);
    drop(reopened);
    drop(ledger);
    fs::remove_dir_all(temp_root).expect("remove temporary database directory");
}
