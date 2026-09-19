//! Compiled-in SQLite schema migrations.
//!
//! Migrations deliberately do not use a third-party migration framework.  The
//! schema and `user_version` update are run by [`migrate`] in one immediate
//! transaction by the storage opener.

use rusqlite::{Connection, Result, TransactionBehavior};

pub const LATEST_SCHEMA_VERSION: u32 = 10;

struct Migration {
    version: u32,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("schema/0001_initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("schema/0002_usage_ledger.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("schema/0003_normalized_token_usage.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("schema/0004_metadata_parent_v2_cleanup.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("schema/0005_project_kind.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("schema/0006_subagent_agent_path.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("schema/0007_usage_context_and_estimated_cost.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("schema/0008_session_resilience.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("schema/0009_skill_usage_events.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("schema/0010_metadata_fact_ordering.sql"),
    },
];

/// Return the schema version supported by this binary.
pub const fn latest_schema_version() -> u32 {
    LATEST_SCHEMA_VERSION
}

/// Apply all migrations after `current_version` atomically.
///
/// The caller must have checked that `current_version` is not newer than the
/// binary.  `BEGIN IMMEDIATE`, the migration SQL, and `PRAGMA user_version`
/// are intentionally kept in the same transaction so a failure cannot leave
/// a partially upgraded database.
pub fn migrate(conn: &mut Connection, current_version: u32) -> Result<u32> {
    if current_version > LATEST_SCHEMA_VERSION {
        return Ok(current_version);
    }

    let crosses_v5 = current_version < 5;
    let foreign_keys_enabled: bool = conn.pragma_query_value(None, "foreign_keys", |row| {
        let value: i64 = row.get(0)?;
        Ok(value != 0)
    })?;
    let foreign_keys_were_disabled = crosses_v5 && foreign_keys_enabled;
    if foreign_keys_were_disabled {
        conn.pragma_update(None, "foreign_keys", false)?;
    }

    let migration_result = (|| {
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut version = current_version;
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version > current_version)
        {
            transaction.execute_batch(migration.sql)?;
            transaction.execute_batch(&format!("PRAGMA user_version = {};", migration.version))?;
            version = migration.version;
        }
        if crosses_v5 {
            let mut statement = transaction.prepare("PRAGMA foreign_key_check")?;
            let mut rows = statement.query([])?;
            if rows.next()?.is_some() {
                return Err(rusqlite::Error::InvalidParameterName(
                    "foreign key check failed after migration".to_owned(),
                ));
            }
            drop(rows);
            drop(statement);
        }
        transaction.commit()?;
        Ok(version)
    })();

    if foreign_keys_were_disabled {
        conn.pragma_update(None, "foreign_keys", true)?;
    }
    migration_result
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::{Connection, params};

    use super::*;

    type SourceStateRow = (
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
        String,
        String,
    );

    fn v2_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0002_usage_ledger.sql"))
            .unwrap();
        connection
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        connection
    }

    struct TestDatabase(PathBuf);

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn file_v2_connection() -> (TestDatabase, Connection) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi_t_dc_028_{}_{}.sqlite",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0002_usage_ledger.sql"))
            .unwrap();
        connection
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        (TestDatabase(path), connection)
    }

    fn file_v3_connection_with_rows() -> (TestDatabase, Connection) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi_t_s04_s05_v3_{}_{}.sqlite",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0002_usage_ledger.sql"))
            .unwrap();
        add_v2_rows(&connection);
        connection
            .execute_batch(include_str!("schema/0003_normalized_token_usage.sql"))
            .unwrap();
        connection
            .execute_batch("PRAGMA user_version = 3;")
            .unwrap();
        (TestDatabase(path), connection)
    }

    fn v5_connection_with_rows() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0002_usage_ledger.sql"))
            .unwrap();
        add_v2_rows(&connection);
        connection
            .execute_batch(include_str!("schema/0003_normalized_token_usage.sql"))
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0004_metadata_parent_v2_cleanup.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0005_project_kind.sql"))
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .pragma_update(None, "user_version", 5_i64)
            .unwrap();
        connection
    }

    fn add_v2_identity(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO threads(
                    thread_id,parent_thread_id,root_session_id,agent_role,archived,
                    metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('root',NULL,'root','main',0,'complete',1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_files(
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (1,'root','/tmp/v2-rollout.jsonl','sessions',1,1,1,100,1,'present',1)",
                [],
            )
            .unwrap();
    }

    fn add_v2_rows(connection: &Connection) {
        add_v2_identity(connection);
        connection
            .execute(
                "INSERT INTO usage_events(
                    ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                    turn_key,model,input_tokens,cached_input_tokens,cache_write_input_tokens,
                    cache_write_status,output_tokens,reasoning_output_tokens,total_tokens,
                    quality_status,source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms
                 ) VALUES (1,'known','normal',10,'root','root','turn','gpt',100,20,5,'known',10,2,110,'complete',1,1,0,10,1),
                          (1,'unknown','normal',11,'root','root','turn','gpt',50,10,NULL,'unknown_missing',5,1,55,'partial',1,1,10,20,1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_event_occurrences(
                    ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms
                 ) VALUES (1,1,1,0,10,'known',1),(1,1,1,10,20,'unknown',1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO turns(
                    ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,
                    started_at_ms,ended_at_ms,start_offset,end_offset,status,
                    start_total_input_tokens,start_total_cached_input_tokens,start_total_cache_write_input_tokens,
                    start_total_output_tokens,start_total_reasoning_output_tokens,start_total_reported_total_tokens,
                    start_total_derived_total_tokens,start_total_cache_write_status,start_total_fingerprint,
                    last_total_input_tokens,last_total_cached_input_tokens,last_total_cache_write_input_tokens,
                    last_total_output_tokens,last_total_reasoning_output_tokens,last_total_reported_total_tokens,
                    last_total_derived_total_tokens,last_total_cache_write_status,last_total_fingerprint,
                    accounted_input_tokens,accounted_cached_input_tokens,accounted_cache_write_input_tokens,
                    accounted_output_tokens,accounted_reasoning_output_tokens,accounted_reported_total_tokens,
                    accounted_derived_total_tokens,accounted_cache_write_status,accounted_fingerprint,
                    accounted_candidate_count,model_state,single_model,unresolved_model_seen,compensation_allowed,
                    block_start_missing,block_time_missing,block_reset,block_ownership_gap,block_parser_gap,
                    block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms
                 ) VALUES (1,1,1,'turn','root','turn',1,20,0,20,'completed',
                    100,20,5,10,2,110,110,'known',zeroblob(32),
                    100,20,5,10,2,110,110,'known',zeroblob(32),
                    10,2,1,1,0,11,11,'known',zeroblob(32),1,'single','gpt',0,1,
                    0,0,0,0,0,0,0,'complete',20,1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_anomalies(
                    ledger_epoch,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
                    file_generation,source_start_offset,anomaly_type,severity,details_json,resolved
                 ) VALUES (1,'ordinary',1,1,'root',1,1,0,'TOTAL_CHAIN_RESET','warning','{}',0),
                          (1,'capability',1,1,'root',1,1,0,'CACHE_WRITE_CAPABILITY_CONFLICT','warning','{}',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_source_states(
                    ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,
                    canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
                    raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
                    previous_total_input_tokens,previous_total_cached_input_tokens,
                    previous_total_cache_write_input_tokens,previous_total_output_tokens,
                    previous_total_reasoning_output_tokens,previous_total_reported_total_tokens,
                    previous_total_derived_total_tokens,previous_total_cache_write_status,
                    previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
                    active_turn_key,active_model,active_model_offset,updated_at_ms
                 ) VALUES (1,1,1,1,1,2,2,20,20,'none',NULL,'root','root','owning_live',
                           100,20,5,10,2,110,110,'known',zeroblob(32),10,'continuous',NULL,'turn','gpt',10,1)",
                [],
            )
            .unwrap();
    }

    fn v2_table_columns(connection: &Connection, table: &str) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap();
        statement
            .query_map([table], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn assert_failed_v2_snapshot(connection: &Connection) {
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let formal_tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN (
                    'usage_events','usage_event_occurrences','turns','usage_source_states','ingest_anomalies'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(formal_tables, 5);
        assert!(
            v2_table_columns(connection, "usage_events")
                .iter()
                .any(|column| column == "cached_input_tokens")
        );
        assert!(
            !v2_table_columns(connection, "usage_events")
                .iter()
                .any(|column| column == "cached_tokens")
        );
        assert!(
            v2_table_columns(connection, "turns")
                .iter()
                .any(|column| column == "start_total_derived_total_tokens")
        );
        assert!(
            !v2_table_columns(connection, "turns")
                .iter()
                .any(|column| column == "start_total_total_tokens")
        );
        assert!(
            v2_table_columns(connection, "usage_source_states")
                .iter()
                .any(|column| column == "previous_total_derived_total_tokens")
        );
        assert!(
            !v2_table_columns(connection, "usage_source_states")
                .iter()
                .any(|column| column == "previous_total_total_tokens")
        );

        let counts: (i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM usage_event_occurrences),
                    (SELECT count(*) FROM turns),
                    (SELECT count(*) FROM usage_source_states),
                    (SELECT count(*) FROM ingest_anomalies)",
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
        assert_eq!(counts, (2, 2, 1, 1, 2));

        let known: (String, i64, i64, Option<i64>, String, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT event_kind,input_tokens,cached_input_tokens,cache_write_input_tokens,
                        cache_write_status,output_tokens,reasoning_output_tokens,total_tokens,source_start_offset
                 FROM usage_events WHERE event_id='known'",
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
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            known,
            (
                "normal".to_owned(),
                100,
                20,
                Some(5),
                "known".to_owned(),
                10,
                2,
                110,
                0
            )
        );
        let unknown: (Option<i64>, String, String, i64, i64) = connection
            .query_row(
                "SELECT cache_write_input_tokens,cache_write_status,quality_status,
                        source_end_offset,created_at_ms
                 FROM usage_events WHERE event_id='unknown'",
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
        assert_eq!(
            unknown,
            (
                None,
                "unknown_missing".to_owned(),
                "partial".to_owned(),
                20,
                1
            )
        );

        let occurrences: Vec<(i64, i64, i64, i64, i64, String)> = connection
            .prepare(
                "SELECT ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id FROM usage_event_occurrences ORDER BY source_start_offset",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            occurrences,
            vec![
                (1, 1, 1, 0, 10, "known".to_owned()),
                (1, 1, 1, 10, 20, "unknown".to_owned())
            ]
        );
        let orphan_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM usage_event_occurrences o
                 LEFT JOIN usage_events e ON e.ledger_epoch=o.ledger_epoch AND e.event_id=o.event_id
                 WHERE e.event_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0);

        let turn_start: (String, i64, i64, Option<i64>, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT turn_key,start_total_input_tokens,start_total_cached_input_tokens,
                        start_total_cache_write_input_tokens,start_total_output_tokens,
                        start_total_reasoning_output_tokens,start_total_reported_total_tokens,
                        start_total_derived_total_tokens
                 FROM turns",
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
            turn_start,
            ("turn".to_owned(), 100, 20, Some(5), 10, 2, 110, 110,)
        );
        let turn_last: (i64, i64, Option<i64>, i64, i64, i64) = connection
            .query_row(
                "SELECT last_total_input_tokens,last_total_cached_input_tokens,
                        last_total_cache_write_input_tokens,last_total_output_tokens,
                        last_total_reasoning_output_tokens,last_total_derived_total_tokens
                 FROM turns",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(turn_last, (100, 20, Some(5), 10, 2, 110));
        let accounted: (i64, i64, Option<i64>, i64, i64, i64, i64, String) = connection
            .query_row(
                "SELECT accounted_input_tokens,accounted_cached_input_tokens,
                        accounted_cache_write_input_tokens,accounted_output_tokens,
                        accounted_reasoning_output_tokens,accounted_reported_total_tokens,
                        accounted_derived_total_tokens,quality_status
                 FROM turns",
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
            accounted,
            (10, 2, Some(1), 1, 0, 11, 11, "complete".to_owned())
        );

        let source_state: SourceStateRow = connection
            .query_row(
                "SELECT usage_parser_version,canonical_algorithm_version,resolved_through_offset,
                        previous_total_input_tokens,previous_total_cached_input_tokens,
                        previous_total_cache_write_input_tokens,previous_total_reported_total_tokens,
                        previous_total_derived_total_tokens,chain_state,active_turn_key
                 FROM usage_source_states",
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
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            source_state,
            (
                2,
                2,
                20,
                100,
                20,
                Some(5),
                Some(110),
                110,
                "continuous".to_owned(),
                "turn".to_owned()
            )
        );

        let anomalies: Vec<(String, String, i64)> = connection
            .prepare(
                "SELECT anomaly_id,anomaly_type,resolved FROM ingest_anomalies ORDER BY anomaly_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            anomalies,
            vec![
                (
                    "capability".to_owned(),
                    "CACHE_WRITE_CAPABILITY_CONFLICT".to_owned(),
                    0
                ),
                ("ordinary".to_owned(), "TOTAL_CHAIN_RESET".to_owned(), 0),
            ]
        );

        let sentinel: String = connection
            .query_row("SELECT sentinel_marker FROM turns_v3", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sentinel, "preexisting migration-failure sentinel");
        let half_migration_tables: Vec<String> = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name IN (
                    'usage_events_v3','usage_event_occurrences_v3','usage_source_states_v3'
                ) ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(half_migration_tables.is_empty());
    }

    fn install_v1(connection: &mut Connection) {
        connection
            .execute_batch(include_str!("schema/0001_initial.sql"))
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
    }

    fn v3_connection_with_rows() -> Connection {
        let connection = v2_connection();
        add_v2_rows(&connection);
        connection
            .execute_batch(include_str!("schema/0003_normalized_token_usage.sql"))
            .unwrap();
        connection
            .execute_batch("PRAGMA user_version = 3;")
            .unwrap();
        connection
    }

    fn seed_v3_metadata_fixture(connection: &Connection) {
        connection
            .execute(
                "UPDATE app_meta SET
                    metadata_parser_version=7,
                    data_revision=8,
                    status_revision=9,
                    scan_state='failed',
                    last_finished_scan_id='scan-old',
                    last_finished_scan_result='failed',
                    last_scan_started_at_ms=1,
                    last_scan_completed_at_ms=2,
                    last_scan_failed_at_ms=3,
                    last_scan_error_code='LEGACY_ERROR',
                    followup_scan_id='followup-old',
                    followup_state='start_failed',
                    followup_trigger='Manual',
                    followup_requested_at_ms=4,
                    followup_enqueued_status_revision=5,
                    followup_error_code='FOLLOWUP_ERROR',
                    last_full_import_completed_at_ms=10,
                    codex_home_fingerprint='old-fingerprint',
                    source_binding_status='ready',
                    usage_active_epoch=1,
                    usage_build_epoch=2,
                    usage_parser_version=3,
                    usage_build_parser_version=3
                 WHERE id=1",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_checkpoints(
                    source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                    processing_status,last_successful_scan_at_ms
                 ) VALUES (1,'metadata',1,64,X'01','ready',10)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO rollout_metadata_facts(
                    source_file_id,file_generation,metadata_parser_version,
                    resolved_through_offset,owning_thread_id,continuation_state,
                    cwd,cwd_provenance,cwd_record_offset,created_at_ms,
                    latest_context_model,latest_context_at_ms,
                    parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,
                    agent_role_hint,agent_role_provenance,agent_role_record_offset,
                    replay_start_offset,owning_records_start_offset,
                    ownership_confidence,fact_quality_status,updated_at_ms
                 ) VALUES (
                    1,1,1,64,'root','owning_live',
                    '/tmp','session_meta',4,1,
                    'gpt',2,'parent','subagent_source',6,
                    'main','session_meta_role',8,0,0,
                    'confirmed','complete',10
                 )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_build_sources(
                    build_epoch,source_file_id,target_parser_version,
                    expected_file_generation,expected_device_id,expected_inode,
                    expected_owning_thread_id,expected_root_session_id,
                    active_committed_offset,active_guard_hash,active_state_fingerprint,
                    required_generation,required_through_offset,observed_raw_size,
                    raw_tail_status,raw_tail_start_offset,membership_reason,
                    completion_status,completion_error_code,completed_generation,
                    completed_through_offset,carry_from_epoch,carry_phase,
                    carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,
                    created_at_ms,updated_at_ms
                 ) VALUES (
                    2,1,3,1,1,1,'root','root',20,NULL,NULL,
                    1,100,100,'none',NULL,'active_contributor','rebuilt',NULL,
                    1,100,NULL,'none',NULL,NULL,NULL,1,1
                 )",
                [],
            )
            .unwrap();
    }

    fn insert_v1_thread_and_source(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO threads (
                    thread_id,parent_thread_id,root_session_id,agent_role,
                    archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('thread',NULL,'thread','main',0,'complete',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_files (
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (1,'thread','/tmp/rollout.jsonl','sessions',1,1,1,100,1,'present',1)",
                [],
            )
            .unwrap();
    }

    fn insert_thread_and_source(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO threads (
                    thread_id,parent_thread_id,root_session_id,agent_role,
                    title,project_name,project_path,project_kind,metadata_model,
                    archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('thread',NULL,'thread','main',NULL,NULL,NULL,'unknown',NULL,
                    0,'complete',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_files (
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (1,'thread','/tmp/rollout.jsonl','sessions',1,1,1,100,1,'present',1)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn v1_upgrade_preserves_metadata_and_installs_metadata_fact_ordering_schema() {
        let mut connection = Connection::open_in_memory().unwrap();
        install_v1(&mut connection);
        connection
            .execute(
                "UPDATE app_meta SET metadata_parser_version=7,data_revision=8,
                    status_revision=9,last_full_import_completed_at_ms=10 WHERE id=1",
                [],
            )
            .unwrap();
        insert_v1_thread_and_source(&connection);

        assert_eq!(migrate(&mut connection, 1).unwrap(), 10);
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        let metadata: (i64, i64, i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT data_revision,status_revision,usage_active_epoch,usage_build_epoch,
                    usage_parser_version,usage_build_parser_version FROM app_meta WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(metadata, (8, 9, 0, None, 0, None));
        for dead_column in [
            "metadata_parser_version",
            "last_full_import_completed_at_ms",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('app_meta') WHERE name=?1",
                    [dead_column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 0, "dead app_meta column remains: {dead_column}");
        }

        for table in [
            "usage_events",
            "usage_event_occurrences",
            "turns",
            "ingest_anomalies",
            "usage_source_states",
            "usage_build_sources",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "missing {table}");
        }
        let index_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='index' AND name IN (
                    'usage_events_time_idx','usage_events_thread_time_idx',
                    'usage_events_root_time_idx','usage_events_model_time_idx',
                    'usage_event_occurrences_event_idx','usage_event_occurrences_source_idx',
                    'usage_build_sources_status_idx')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 7);
        let occurrence_foreign_keys: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_foreign_key_list('usage_event_occurrences')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(occurrence_foreign_keys, 3);
    }

    #[test]
    fn t_s01_001_project_kind_migration_backfills_and_rejects_invalid_values() {
        let mut connection = v3_connection_with_rows();
        connection
            .execute(
                "INSERT INTO threads (
                    thread_id,root_session_id,agent_role,title,project_name,project_path,
                    archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES
                    ('path-thread','path-thread','main','Path title','Path name','/tmp/path',0,'complete',1),
                    ('empty-thread','empty-thread','main','Empty title','Empty name','',0,'complete',1)",
                [],
            )
            .unwrap();
        let usage_count_before: i64 = connection
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();

        assert_eq!(migrate(&mut connection, 3).unwrap(), 10);
        let kinds: Vec<(String, String, String)> = connection
            .prepare(
                "SELECT project_kind,project_path,project_name FROM threads
                 WHERE thread_id IN ('path-thread','empty-thread') ORDER BY thread_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            kinds,
            vec![
                ("unknown".to_owned(), "".to_owned(), "Empty name".to_owned()),
                (
                    "project".to_owned(),
                    "/tmp/path".to_owned(),
                    "Path name".to_owned()
                ),
            ]
        );
        let usage_count_after: i64 = connection
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count_after, usage_count_before);
        let foreign_keys_enabled: i64 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys_enabled, 1);
        let mut foreign_key_check = connection.prepare("PRAGMA foreign_key_check").unwrap();
        let mut foreign_key_rows = foreign_key_check.query([]).unwrap();
        assert!(foreign_key_rows.next().unwrap().is_none());
        drop(foreign_key_rows);
        drop(foreign_key_check);
        assert!(
            connection
                .execute(
                    "INSERT INTO threads(
                    thread_id,root_session_id,agent_role,project_kind,archived,
                    metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('invalid','invalid','main','not-a-kind',0,'complete',1)",
                    [],
                )
                .is_err()
        );
        assert_eq!(migrate(&mut connection, 10).unwrap(), 10);
    }

    #[test]
    fn migration_failure_rolls_back_schema_and_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        install_v1(&mut connection);
        connection
            .execute("CREATE TABLE usage_events(conflict INTEGER)", [])
            .unwrap();
        assert!(migrate(&mut connection, 1).is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let foreign_keys_enabled: i64 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys_enabled, 1);
        let usage_column_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('app_meta')
                 WHERE name='usage_active_epoch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(usage_column_count, 0);
        let conflict_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('usage_events')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(conflict_columns, 1);
    }

    #[test]
    fn usage_epoch_checkpoint_and_canonical_constraints_matrix() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection, 0).unwrap();
        insert_thread_and_source(&connection);

        for sql in [
            "UPDATE app_meta SET usage_active_epoch=-1 WHERE id=1",
            "UPDATE app_meta SET usage_parser_version=-1 WHERE id=1",
            "UPDATE app_meta SET usage_build_epoch=1 WHERE id=1",
            "UPDATE app_meta SET usage_build_parser_version=1 WHERE id=1",
            "UPDATE app_meta SET usage_build_epoch=2,usage_build_parser_version=1 WHERE id=1",
        ] {
            assert!(connection.execute(sql, []).is_err(), "accepted {sql}");
        }
        connection
            .execute(
                "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=1,
                    usage_build_epoch=2,usage_build_parser_version=2 WHERE id=1",
                [],
            )
            .unwrap();

        connection
            .execute(
                "INSERT INTO source_checkpoints(
                    source_file_id,consumer_kind,parser_version,committed_offset,processing_status
                 ) VALUES (1,'usage',2,0,'rebuild_required')",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO source_checkpoints(
                        source_file_id,consumer_kind,parser_version,committed_offset,processing_status
                     ) VALUES (1,'usage',2,0,'ready')",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE source_checkpoints SET processing_status='build'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .is_err()
        );

        connection
            .execute(
                "UPDATE app_meta SET usage_build_epoch=NULL,usage_build_parser_version=NULL WHERE id=1",
                [],
            )
            .unwrap();
        for (epoch, event_id, start_offset) in [(1, "active", 0), (2, "inactive", 1)] {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                        reasoning_tokens,total_tokens,quality_status,source_file_id,file_generation,
                        source_start_offset,source_end_offset,created_at_ms
                     ) VALUES (?1,?2,'normal',1,'thread','thread','model',NULL,NULL,10,2,3,4,1,14,
                        'complete',1,1,?3,?3+1,1)",
                    params![epoch, event_id, start_offset],
                )
                .unwrap();
        }
        let active_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 1);

        for sql in [
            "UPDATE usage_events SET input_tokens=-1 WHERE event_id='active'",
            "UPDATE usage_events SET cached_tokens=11 WHERE event_id='active'",
            "UPDATE usage_events SET cache_write_tokens=9 WHERE event_id='active'",
            "UPDATE usage_events SET reasoning_tokens=5 WHERE event_id='active'",
            "UPDATE usage_events SET total_tokens=13 WHERE event_id='active'",
        ] {
            assert!(connection.execute(sql, []).is_err(), "accepted {sql}");
        }
    }

    #[test]
    fn t_dc_026_fresh_schema_has_only_canonical_columns() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        assert_eq!(migrate(&mut connection, 0).unwrap(), 10);
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        for (table, required, forbidden) in [
            (
                "usage_events",
                vec![
                    "input_tokens",
                    "cached_tokens",
                    "cache_write_tokens",
                    "output_tokens",
                    "reasoning_tokens",
                    "total_tokens",
                ],
                vec![
                    "cached_input_tokens",
                    "cache_write_input_tokens",
                    "cache_write_status",
                    "reasoning_output_tokens",
                    "cache_tokens",
                ],
            ),
            (
                "turns",
                vec![
                    "start_total_cached_tokens",
                    "last_total_cached_tokens",
                    "accounted_cached_tokens",
                ],
                vec![
                    "start_total_cached_input_tokens",
                    "last_total_cache_write_status",
                    "accounted_derived_total_tokens",
                ],
            ),
            (
                "usage_source_states",
                vec![
                    "previous_total_cached_tokens",
                    "previous_total_cache_write_tokens",
                ],
                vec![
                    "previous_total_cached_input_tokens",
                    "previous_total_cache_write_status",
                ],
            ),
        ] {
            let mut statement = connection
                .prepare("SELECT name FROM pragma_table_info(?1)")
                .unwrap();
            let names = statement
                .query_map([table], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            for name in required {
                assert!(names.iter().any(|value| value == name), "{table}.{name}");
            }
            for name in forbidden {
                assert!(!names.iter().any(|value| value == name), "{table}.{name}");
            }
        }
        let metadata_columns = v2_table_columns(&connection, "rollout_metadata_facts");
        assert!(
            metadata_columns
                .iter()
                .any(|name| name == "latest_context_turn_id")
        );
        assert!(
            metadata_columns
                .iter()
                .any(|name| name == "relationship_conflict")
        );
        for dead_column in [
            "metadata_parser_version",
            "last_full_import_completed_at_ms",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('app_meta') WHERE name=?1",
                    [dead_column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 0, "dead app_meta column remains: {dead_column}");
        }

        insert_thread_and_source(&connection);
        connection
            .execute(
                "INSERT INTO rollout_metadata_facts(
                    source_file_id,file_generation,metadata_parser_version,
                    resolved_through_offset,owning_thread_id,continuation_state,
                    parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,
                    ownership_confidence,fact_quality_status,updated_at_ms
                 ) VALUES (1,1,1,0,'thread','owning_live','parent',
                    'session_meta_parent',0,'confirmed','complete',0)",
                [],
            )
            .unwrap();
        let parent_provenance: String = connection
            .query_row(
                "SELECT parent_hint_provenance FROM rollout_metadata_facts WHERE source_file_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent_provenance, "session_meta_parent");
        let defaults: (Option<String>, i64) = connection
            .query_row(
                "SELECT latest_context_turn_id, relationship_conflict
                 FROM rollout_metadata_facts WHERE source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(defaults, (None, 0));
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET continuation_state='replayed_ancestor', ownership_confidence='confirmed'
                 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT continuation_state FROM rollout_metadata_facts WHERE source_file_id=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "replayed_ancestor"
        );
        assert!(
            connection
                .execute(
                    "UPDATE rollout_metadata_facts SET ownership_confidence='unresolved'
                     WHERE source_file_id=1",
                    [],
                )
                .is_err()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type='table' AND name='usage_session_quarantine'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type='index' AND name='rollout_metadata_facts_thread_idx'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_foreign_key_list('rollout_metadata_facts')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn m4_02_v3_to_v4_preserves_real_shaped_metadata_and_usage_state() {
        let mut connection = v3_connection_with_rows();
        seed_v3_metadata_fixture(&connection);

        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('app_meta')
                     WHERE name IN ('metadata_parser_version','last_full_import_completed_at_ms')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(migrate(&mut connection, 3).unwrap(), 10);
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );

        let revisions: (i64, i64) = connection
            .query_row(
                "SELECT data_revision,status_revision FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(revisions, (8, 9));
        let scan: (String, String, String, i64, i64, i64, String) = connection
            .query_row(
                "SELECT scan_state,last_finished_scan_id,last_finished_scan_result,
                    last_scan_started_at_ms,last_scan_completed_at_ms,last_scan_failed_at_ms,
                    last_scan_error_code FROM app_meta WHERE id=1",
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
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            scan,
            (
                "failed".to_owned(),
                "scan-old".to_owned(),
                "failed".to_owned(),
                1,
                2,
                3,
                "LEGACY_ERROR".to_owned()
            )
        );
        let active_scan_id: Option<String> = connection
            .query_row(
                "SELECT active_scan_id FROM app_meta WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_scan_id, None);
        let followup: (String, String, String, i64, i64, String) = connection
            .query_row(
                "SELECT followup_scan_id,followup_state,followup_trigger,
                    followup_requested_at_ms,followup_enqueued_status_revision,
                    followup_error_code FROM app_meta WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            followup,
            (
                "followup-old".to_owned(),
                "start_failed".to_owned(),
                "Manual".to_owned(),
                4,
                5,
                "FOLLOWUP_ERROR".to_owned()
            )
        );
        let binding: (String, String) = connection
            .query_row(
                "SELECT codex_home_fingerprint,source_binding_status FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(binding, ("old-fingerprint".to_owned(), "ready".to_owned()));
        let usage_epochs: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version,
                    usage_build_parser_version FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(usage_epochs, (1, 2, 3, 3));

        let fact: (i64, i64, String, String, i64) = connection
            .query_row(
                "SELECT metadata_parser_version,resolved_through_offset,
                    owning_thread_id,parent_hint_provenance,parent_hint_record_offset
                 FROM rollout_metadata_facts WHERE source_file_id=1",
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
        assert_eq!(
            fact,
            (1, 64, "root".to_owned(), "subagent_source".to_owned(), 6)
        );
        let checkpoint: (i64, i64, String) = connection
            .query_row(
                "SELECT parser_version,committed_offset,processing_status
                 FROM source_checkpoints WHERE source_file_id=1 AND consumer_kind='metadata'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(checkpoint, (1, 64, "ready".to_owned()));
        let source_state: (i64, i64, String, String) = connection
            .query_row(
                "SELECT ledger_epoch,usage_parser_version,chain_state,active_turn_key
                 FROM usage_source_states WHERE source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            source_state,
            (1, 2, "continuous".to_owned(), "turn".to_owned())
        );
        let known: (i64, i64, Option<i64>, i64) = connection
            .query_row(
                "SELECT input_tokens,cached_tokens,cache_write_tokens,total_tokens
                 FROM usage_events WHERE event_id='known'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(known, (100, 20, Some(5), 110));
        let build: (i64, i64, i64, String, String, i64) = connection
            .query_row(
                "SELECT build_epoch,source_file_id,target_parser_version,
                    raw_tail_status,completion_status,completed_through_offset
                 FROM usage_build_sources WHERE build_epoch=2 AND source_file_id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            build,
            (2, 1, 3, "none".to_owned(), "rebuilt".to_owned(), 100)
        );

        for dead_column in [
            "metadata_parser_version",
            "last_full_import_completed_at_ms",
        ] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM pragma_table_info('app_meta') WHERE name=?1",
                        [dead_column],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
        }
        connection
            .execute(
                "UPDATE rollout_metadata_facts SET parent_hint_provenance='session_meta_parent'
                 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT parent_hint_provenance FROM rollout_metadata_facts WHERE source_file_id=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "session_meta_parent"
        );
    }

    #[test]
    fn m4_03_failed_v3_to_v4_migration_rolls_back_atomically() {
        let mut connection = v3_connection_with_rows();
        seed_v3_metadata_fixture(&connection);
        connection
            .execute(
                "CREATE TABLE app_meta_v4 (sentinel_marker TEXT NOT NULL)",
                [],
            )
            .unwrap();

        assert!(migrate(&mut connection, 3).is_err());
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('app_meta')
                     WHERE name IN ('metadata_parser_version','last_full_import_completed_at_ms')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        let fact: (i64, String) = connection
            .query_row(
                "SELECT metadata_parser_version,parent_hint_provenance
                 FROM rollout_metadata_facts WHERE source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(fact, (1, "subagent_source".to_owned()));
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='rollout_metadata_facts_v4'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='app_meta_v4'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert!(
            connection
                .execute(
                    "UPDATE rollout_metadata_facts SET parent_hint_provenance='session_meta_parent'
                 WHERE source_file_id=1",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn s4_s5_runtime_reads_upgraded_v3_database_without_dead_column_errors() {
        let (database, connection) = file_v3_connection_with_rows();
        seed_v3_metadata_fixture(&connection);
        let portable_source_path = std::env::temp_dir()
            .join("usagi-s04-s05-v3-rollout.jsonl")
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                "UPDATE source_files SET current_path = ?1 WHERE source_file_id = 1",
                [&portable_source_path],
            )
            .unwrap();
        drop(connection);

        let codex_home = std::env::temp_dir().join(format!(
            "usagi_t_s04_s05_codex_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ledger = crate::storage::Ledger::open(crate::storage::LedgerOptions::new(
            &database.0,
            &codex_home,
        ))
        .unwrap();
        assert_eq!(ledger.schema_version().unwrap(), 10);
        let app_state = ledger.app_state().unwrap();
        assert_eq!(app_state.data_revision, 9);
        assert_eq!(app_state.scan.status_revision, 10);
        assert_eq!(
            app_state.scan.last_finished_scan_id.as_deref(),
            Some("scan-old")
        );
        assert_eq!(
            app_state.scan.followup_scan_id.as_deref(),
            Some("followup-old")
        );
        assert_eq!(
            app_state.scan.followup_state,
            Some(crate::domain::FollowupState::StartFailed)
        );
        assert_eq!(
            ledger.load_metadata_scan_state([1]).unwrap().entries.len(),
            1
        );
    }

    #[test]
    fn t_dc_027_v2_rows_migrate_without_losing_canonical_values_or_occurrences() {
        let mut connection = v2_connection();
        add_v2_rows(&connection);
        assert_eq!(migrate(&mut connection, 2).unwrap(), 10);
        let known: (i64, i64, Option<i64>, i64, i64, i64) = connection
            .query_row(
                "SELECT input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens FROM usage_events WHERE event_id='known'",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(known, (100, 20, Some(5), 10, 2, 110));
        let unknown: Option<i64> = connection
            .query_row(
                "SELECT cache_write_tokens FROM usage_events WHERE event_id='unknown'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unknown, None);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_event_occurrences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        let occurrence_parent: String = connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('usage_event_occurrences') WHERE id=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(occurrence_parent, "usage_events");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_event_occurrences o
                     LEFT JOIN usage_events e ON e.ledger_epoch=o.ledger_epoch AND e.event_id=o.event_id
                     WHERE e.event_id IS NULL",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM ingest_anomalies WHERE anomaly_type='TOTAL_CHAIN_RESET'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(connection.query_row("SELECT count(*) FROM ingest_anomalies WHERE anomaly_type='CACHE_WRITE_CAPABILITY_CONFLICT'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        let previous: (i64, i64, Option<i64>) = connection.query_row("SELECT previous_total_input_tokens,previous_total_cached_tokens,previous_total_cache_write_tokens FROM usage_source_states", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(previous, (100, 20, Some(5)));
        let turn: (i64, i64, Option<i64>, i64, i64, i64) = connection.query_row(
            "SELECT start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens FROM turns",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).unwrap();
        assert_eq!(turn, (100, 20, Some(5), 10, 2, 110));
    }

    #[test]
    fn t_dc_028_failed_migration_rolls_back_to_v2_schema_and_version() {
        let (database, mut connection) = file_v2_connection();
        add_v2_rows(&connection);
        // This pre-existing object forces CREATE TABLE turns_v3 to fail. Its
        // marker lets the rollback checks distinguish the sentinel from a
        // table created by the migration.
        connection
            .execute_batch(
                "CREATE TABLE turns_v3 (sentinel_marker TEXT NOT NULL);
                 INSERT INTO turns_v3(sentinel_marker)
                 VALUES ('preexisting migration-failure sentinel');",
            )
            .unwrap();
        assert!(migrate(&mut connection, 2).is_err());
        assert_failed_v2_snapshot(&connection);
        drop(connection);

        let reopened = Connection::open(&database.0).unwrap();
        reopened.pragma_update(None, "foreign_keys", true).unwrap();
        assert_failed_v2_snapshot(&reopened);
    }

    #[test]
    fn t_dc_029_v2_parser_and_canonical_versions_are_not_promoted() {
        let mut connection = v2_connection();
        add_v2_rows(&connection);
        connection
            .execute(
                "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=2 WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(migrate(&mut connection, 2).unwrap(), 10);
        let versions: (i64, i64) = connection.query_row("SELECT app_meta.usage_parser_version,usage_source_states.canonical_algorithm_version FROM app_meta JOIN usage_source_states ON usage_source_states.ledger_epoch=app_meta.usage_active_epoch", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(versions, (2, 2));
        assert_eq!(crate::usage::normalized::canonical_algorithm_for(2), None);
    }

    #[test]
    fn t_mu03_s01_v7_features_survive_v8_upgrade_idempotence_and_rollback() {
        let mut fresh = Connection::open_in_memory().unwrap();
        fresh.pragma_update(None, "foreign_keys", true).unwrap();
        assert_eq!(migrate(&mut fresh, 0).unwrap(), 10);
        assert_eq!(
            fresh
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );

        for (table, required) in [
            (
                "usage_events",
                ["reasoning_effort", "estimated_cost_nanos_usd"].as_slice(),
            ),
            (
                "usage_source_states",
                ["active_reasoning_effort", "active_reasoning_effort_offset"].as_slice(),
            ),
            (
                "turns",
                [
                    "reasoning_effort_state",
                    "single_reasoning_effort",
                    "unresolved_reasoning_effort_seen",
                ]
                .as_slice(),
            ),
            (
                "app_meta",
                ["cost_algorithm_version", "pricing_catalog_version"].as_slice(),
            ),
            (
                "rollout_metadata_facts",
                [
                    "agent_path",
                    "agent_path_provenance",
                    "agent_path_record_offset",
                ]
                .as_slice(),
            ),
        ] {
            let columns = v2_table_columns(&fresh, table);
            for name in required {
                assert!(
                    columns.iter().any(|column| column == name),
                    "{table}.{name}"
                );
            }
        }
        let app_cost_versions: (i64, i64) = fresh
            .query_row(
                "SELECT cost_algorithm_version,pricing_catalog_version FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(app_cost_versions, (0, 0));
        assert!(
            fresh
                .execute(
                    "UPDATE app_meta SET cost_algorithm_version=-1 WHERE id=1",
                    []
                )
                .is_err()
        );
        assert!(
            fresh
                .execute(
                    "UPDATE app_meta SET pricing_catalog_version=-1 WHERE id=1",
                    []
                )
                .is_err()
        );

        let source_pk: i64 = fresh
            .query_row(
                "SELECT count(*) FROM pragma_table_info('usage_source_states') WHERE pk>0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let turn_pk: i64 = fresh
            .query_row(
                "SELECT count(*) FROM pragma_table_info('turns') WHERE pk>0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_pk, 2);
        assert_eq!(turn_pk, 4);
        assert_eq!(
            fresh
                .query_row(
                    "SELECT count(*) FROM pragma_foreign_key_list('usage_source_states')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        assert_eq!(
            fresh
                .query_row(
                    "SELECT count(*) FROM pragma_foreign_key_list('turns')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        let usage_sql: String = fresh
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='usage_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(usage_sql.contains("estimated_cost_nanos_usd"));
        assert!(usage_sql.contains("estimated_cost_nanos_usd IS NULL"));
        assert_eq!(migrate(&mut fresh, 10).unwrap(), 10);

        let mut upgraded = v5_connection_with_rows();
        let before: (i64, i64, i64, i64, i64) = upgraded
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM usage_event_occurrences),
                    (SELECT count(*) FROM turns),
                    (SELECT count(*) FROM usage_source_states),
                    (SELECT count(*) FROM ingest_anomalies)",
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
        assert_eq!(migrate(&mut upgraded, 5).unwrap(), 10);
        assert_eq!(
            upgraded
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let after: (i64, i64, i64, i64, i64) = upgraded
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM usage_event_occurrences),
                    (SELECT count(*) FROM turns),
                    (SELECT count(*) FROM usage_source_states),
                    (SELECT count(*) FROM ingest_anomalies)",
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
        assert_eq!(before, after);
        assert_eq!(migrate(&mut upgraded, 10).unwrap(), 10);
        let mut foreign_key_statement = upgraded.prepare("PRAGMA foreign_key_check").unwrap();
        let mut foreign_key_rows = foreign_key_statement.query([]).unwrap();
        let foreign_key_check = foreign_key_rows.next().unwrap();
        assert!(foreign_key_check.is_none());

        let mut failed = v5_connection_with_rows();
        failed
            .execute("CREATE TABLE usage_source_states_v6(sentinel INTEGER)", [])
            .unwrap();
        assert!(migrate(&mut failed, 5).is_err());
        assert_eq!(
            failed
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            failed
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('usage_events') WHERE name='reasoning_effort'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            failed
                .query_row("SELECT count(*) FROM usage_source_states_v6", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            failed
                .query_row("SELECT count(*) FROM usage_source_states", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
    }
}
