use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use usagi::{
    storage::{Ledger, LedgerOptions},
    usage::ledger::UsageLedger,
};

fn temporary_paths() -> (PathBuf, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("usagi-spec01-phase3b-{unique}"));
    fs::create_dir_all(&root).unwrap();
    (root.join("mu.sqlite3"), root.join("codex"))
}

fn insert_thread(connection: &Connection, source: &str, thread_id: &str) {
    connection
        .execute(
            "INSERT INTO threads(
                thread_id,source,native_session_id,parent_thread_id,root_session_id,
                agent_role,title,project_name,project_path,project_kind,metadata_model,
                created_at_ms,updated_at_ms,archived,metadata_quality_status,metadata_resolved_at_ms
             ) VALUES (?1,?2,?1,NULL,?1,'main',NULL,NULL,NULL,'unknown',NULL,0,0,0,'complete',0)",
            params![thread_id, source],
        )
        .unwrap();
}

fn insert_event(
    connection: &Connection,
    source: &str,
    source_epoch: i64,
    event_id: &str,
    thread_id: &str,
    occurred_at_ms: i64,
) {
    connection
        .execute(
            "INSERT INTO usage_events(
                source,source_epoch,event_id,event_kind,occurred_at_ms,
                thread_id,root_session_id,turn_key,model,reasoning_effort,
                estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,
                output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms
             ) VALUES (?1,?2,?3,'normal',?4,?5,?5,NULL,'gpt-5.6-sol',NULL,NULL,
                       1,0,0,1,0,2,'complete',0)",
            params![source, source_epoch, event_id, occurred_at_ms, thread_id],
        )
        .unwrap();
}

#[test]
fn s09_cross_source_gc_keeps_each_active_and_build_epoch() {
    let (db_path, codex_home) = temporary_paths();

    // Bootstrap v11 through the normal Ledger path, then seed source rows
    // using a separate SQLite connection so the test exercises the public
    // cleanup facade rather than private storage helpers.
    let ledger = Ledger::open(LedgerOptions::new(&db_path, &codex_home)).unwrap();
    drop(ledger);

    let connection = Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE source_usage_epochs
             SET active_epoch=2,build_epoch=3,active_parser_version=11,build_parser_version=11
             WHERE source='codex'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_usage_epochs(source,active_epoch,active_parser_version)
             VALUES ('fake-source',8,1)",
            [],
        )
        .unwrap();
    insert_thread(&connection, "codex", "codex-root");
    insert_thread(&connection, "fake-source", "fake-root");
    insert_event(&connection, "codex", 2, "codex-active", "codex-root", 200);
    insert_event(&connection, "codex", 3, "codex-build", "codex-root", 300);
    insert_event(&connection, "codex", 1, "codex-inactive", "codex-root", 100);
    insert_event(
        &connection,
        "fake-source",
        8,
        "fake-active",
        "fake-root",
        800,
    );
    insert_event(
        &connection,
        "fake-source",
        7,
        "fake-inactive",
        "fake-root",
        700,
    );
    drop(connection);

    let ledger = Ledger::open(LedgerOptions::new(&db_path, &codex_home)).unwrap();
    let usage = UsageLedger::new(&ledger);
    let mut deleted = 0;
    for _ in 0..8 {
        let page = usage.cleanup_inactive(1).unwrap();
        deleted += page;
        if page == 0 {
            break;
        }
    }
    assert_eq!(deleted, 2);
    drop(ledger);

    let connection = Connection::open(&db_path).unwrap();
    let rows = connection
        .prepare(
            "SELECT source,source_epoch,event_id FROM usage_events
             ORDER BY source,source_epoch,event_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("codex".to_owned(), 2, "codex-active".to_owned()),
            ("codex".to_owned(), 3, "codex-build".to_owned()),
            ("fake-source".to_owned(), 8, "fake-active".to_owned()),
        ]
    );

    let _ = fs::remove_dir_all(db_path.parent().unwrap());
}
