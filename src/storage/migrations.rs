//! Compiled-in SQLite schema migrations.
//!
//! Migrations deliberately do not use a third-party migration framework.  The
//! schema and `user_version` update are run by [`migrate`] in one immediate
//! transaction by the storage opener.

use rusqlite::{Connection, Result, TransactionBehavior};

pub const LATEST_SCHEMA_VERSION: u32 = 11;

struct Migration {
    version: u32,
    sql: &'static str,
    requires_foreign_keys_off: bool,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("schema/0001_initial.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 2,
        sql: include_str!("schema/0002_usage_ledger.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 3,
        sql: include_str!("schema/0003_normalized_token_usage.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 4,
        sql: include_str!("schema/0004_metadata_parent_v2_cleanup.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 5,
        sql: include_str!("schema/0005_project_kind.sql"),
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 6,
        sql: include_str!("schema/0006_subagent_agent_path.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 7,
        sql: include_str!("schema/0007_usage_context_and_estimated_cost.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 8,
        sql: include_str!("schema/0008_session_resilience.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 9,
        sql: include_str!("schema/0009_skill_usage_events.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 10,
        sql: include_str!("schema/0010_metadata_fact_ordering.sql"),
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 11,
        sql: include_str!("schema/0011_multi_source_core.sql"),
        requires_foreign_keys_off: true,
    },
];

/// Return the schema version supported by this binary.
pub const fn latest_schema_version() -> u32 {
    LATEST_SCHEMA_VERSION
}

fn app_meta_has_column(connection: &Connection, column: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
             SELECT 1 FROM pragma_table_info('app_meta') WHERE name = ?1
         )",
            [column],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
}

fn ensure_app_meta_v11_metadata_columns(connection: &Connection) -> Result<()> {
    if !app_meta_has_column(connection, "metadata_parser_version")? {
        connection.execute_batch(
            "ALTER TABLE app_meta ADD COLUMN metadata_parser_version
                 INTEGER NOT NULL DEFAULT 0 CHECK (metadata_parser_version >= 0);",
        )?;
    }
    if !app_meta_has_column(connection, "last_full_import_completed_at_ms")? {
        connection.execute_batch(
            "ALTER TABLE app_meta ADD COLUMN last_full_import_completed_at_ms
                 INTEGER CHECK (
                     last_full_import_completed_at_ms IS NULL
                     OR last_full_import_completed_at_ms >= 0
                 );",
        )?;
    }
    Ok(())
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

    let foreign_keys_enabled: bool = conn.pragma_query_value(None, "foreign_keys", |row| {
        let value: i64 = row.get(0)?;
        Ok(value != 0)
    })?;
    let requires_foreign_keys_off = MIGRATIONS.iter().any(|migration| {
        migration.version > current_version && migration.requires_foreign_keys_off
    });
    let foreign_keys_were_disabled = requires_foreign_keys_off && foreign_keys_enabled;
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
            if migration.version == 11 {
                ensure_app_meta_v11_metadata_columns(&transaction)?;
            }
            transaction.execute_batch(migration.sql)?;
            transaction.execute_batch(&format!("PRAGMA user_version = {};", migration.version))?;
            version = migration.version;
        }
        if requires_foreign_keys_off {
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
        path::{Path, PathBuf},
        sync::Arc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "usagi-{label}-{}-{suffix}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
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

    fn file_v11_connection() -> (TestDatabase, Connection) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi_t_s01_m07_{}_{}.sqlite",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_file(&path);
        let mut connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        assert_eq!(migrate(&mut connection, 0).unwrap(), 11);
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

    fn v10_connection_with_rows() -> Connection {
        let connection = v5_connection_with_rows();
        connection
            .execute_batch(include_str!("schema/0006_subagent_agent_path.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!(
                "schema/0007_usage_context_and_estimated_cost.sql"
            ))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0008_session_resilience.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0009_skill_usage_events.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("schema/0010_metadata_fact_ordering.sql"))
            .unwrap();
        connection
            .pragma_update(None, "user_version", 10_i64)
            .unwrap();
        connection
    }


    fn file_v10_connection_with_rows() -> (TestDatabase, Connection) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi_t_s01_upgrade_startup_{}_{}.sqlite",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_file(&path);
        let connection = v10_connection_with_rows();
        connection
            .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        drop(connection);
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        (TestDatabase(path), connection)
    }

    fn v10_connection_with_thread_hierarchy() -> Connection {
        let connection = v10_connection_with_rows();
        connection
            .execute(
                "INSERT INTO threads(
                    thread_id,parent_thread_id,root_session_id,agent_role,title,
                    project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES
                    ('subagent-a','root','root','subagent','subagent A','project',0,'complete',2),
                    ('subagent-b','subagent-a','root','subagent','subagent B','project',0,'partial',3)",
                [],
            )
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
                    thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,
                    title,project_name,project_path,project_kind,metadata_model,
                    archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('thread','codex','thread',NULL,'thread','main',NULL,NULL,NULL,'unknown',NULL,
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

        assert_eq!(migrate(&mut connection, 1).unwrap(), 11);
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        let metadata: (i64, i64, i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT data_revision,status_revision,active_epoch,build_epoch,
                    active_parser_version,build_parser_version
                 FROM app_meta JOIN source_usage_epochs ON source_usage_epochs.source='codex'
                 WHERE app_meta.id=1",
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
        let app_meta_metadata: (i64, Option<i64>) = connection
            .query_row(
                "SELECT metadata_parser_version,last_full_import_completed_at_ms
                 FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(app_meta_metadata, (7, Some(10)));

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
        assert_eq!(occurrence_foreign_keys, 4);
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

        assert_eq!(migrate(&mut connection, 3).unwrap(), 11);
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
        assert_eq!(migrate(&mut connection, 11).unwrap(), 11);
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
            "UPDATE source_usage_epochs SET active_epoch=-1 WHERE source='codex'",
            "UPDATE source_usage_epochs SET active_parser_version=-1 WHERE source='codex'",
            "UPDATE source_usage_epochs SET build_epoch=1 WHERE source='codex'",
            "UPDATE source_usage_epochs SET build_parser_version=1 WHERE source='codex'",
            "UPDATE source_usage_epochs SET build_epoch=2,build_parser_version=1 WHERE source='codex'",
        ] {
            assert!(connection.execute(sql, []).is_err(), "accepted {sql}");
        }
        connection
            .execute(
                "UPDATE source_usage_epochs SET active_epoch=1,active_parser_version=1,
                    build_epoch=2,build_parser_version=2 WHERE source='codex'",
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
                "UPDATE source_usage_epochs SET build_epoch=NULL,build_parser_version=NULL WHERE source='codex'",
                [],
            )
            .unwrap();
        for (epoch, event_id, _start_offset) in [(1, "active", 0), (2, "inactive", 1)] {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                        reasoning_tokens,total_tokens,quality_status,created_at_ms
                     ) VALUES ('codex',?1,?2,'normal',1,'thread','thread','model',NULL,NULL,10,2,3,4,1,14,
                        'complete',1)",
                    params![epoch, event_id],
                )
                .unwrap();
        }
        let active_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE source='codex' AND source_epoch=(SELECT active_epoch FROM source_usage_epochs WHERE source='codex')",
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
        assert_eq!(migrate(&mut connection, 0).unwrap(), 11);
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
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
        let app_meta_metadata: (i64, Option<i64>) = connection
            .query_row(
                "SELECT metadata_parser_version,last_full_import_completed_at_ms
                 FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(app_meta_metadata, (0, None));

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
        assert_eq!(migrate(&mut connection, 3).unwrap(), 11);
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            11
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
        let usage_epochs: (i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(usage_epochs, (1, Some(2), 3, Some(3)));

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

        let app_meta_metadata: (i64, Option<i64>) = connection
            .query_row(
                "SELECT metadata_parser_version,last_full_import_completed_at_ms
                 FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(app_meta_metadata, (7, Some(10)));
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
        assert_eq!(ledger.schema_version().unwrap(), 11);
        let app_state = ledger.app_state().unwrap();
        assert_eq!(app_state.data_revision, 8);
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
        assert_eq!(migrate(&mut connection, 2).unwrap(), 11);
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
                     LEFT JOIN usage_events e ON e.source=o.source
                        AND e.source_epoch=o.ledger_epoch AND e.event_id=o.event_id
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
        assert_eq!(migrate(&mut connection, 2).unwrap(), 11);
        let versions: (i64, i64) = connection.query_row("SELECT source_usage_epochs.active_parser_version,usage_source_states.canonical_algorithm_version FROM source_usage_epochs JOIN usage_source_states ON usage_source_states.ledger_epoch=source_usage_epochs.active_epoch WHERE source_usage_epochs.source='codex'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(versions, (2, 2));
        assert_eq!(crate::usage::normalized::canonical_algorithm_for(2), None);
    }

    #[test]
    fn t_mu03_s01_v7_features_survive_v8_upgrade_idempotence_and_rollback() {
        let mut fresh = Connection::open_in_memory().unwrap();
        fresh.pragma_update(None, "foreign_keys", true).unwrap();
        assert_eq!(migrate(&mut fresh, 0).unwrap(), 11);
        assert_eq!(
            fresh
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            11
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
        assert_eq!(migrate(&mut fresh, 11).unwrap(), 11);

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
        assert_eq!(migrate(&mut upgraded, 5).unwrap(), 11);
        assert_eq!(
            upgraded
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            11
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
        assert_eq!(migrate(&mut upgraded, 11).unwrap(), 11);
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

    #[test]
    fn m00_provenance_preflight_backfills_and_rejects_ambiguous_rows() {
        let mut backfill = v10_connection_with_rows();
        backfill
            .execute("DELETE FROM usage_event_occurrences", [])
            .unwrap();
        assert_eq!(migrate(&mut backfill, 10).unwrap(), 11);
        let counts: (i64, i64) = backfill
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM usage_event_occurrences)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 2));

        let mut ambiguous = v10_connection_with_rows();
        ambiguous
            .execute(
                "UPDATE usage_event_occurrences
                 SET source_end_offset=99 WHERE event_id='known'",
                [],
            )
            .unwrap();
        assert!(migrate(&mut ambiguous, 10).is_err());
        assert_eq!(
            ambiguous
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(
            ambiguous
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('usage_events')
                     WHERE name='source_file_id'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            ambiguous
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn m01_thread_identity_migration_preserves_codex_ids_and_checks_native_id() {
        let mut connection = v10_connection_with_thread_hierarchy();
        let before: Vec<(String, Option<String>, Option<String>, String)> = connection
            .prepare(
                "SELECT thread_id,parent_thread_id,root_session_id,agent_role
                 FROM threads ORDER BY thread_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        type ThreadIdentityRow = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        );
        let mut statement = connection
            .prepare(
                "SELECT thread_id,source,native_session_id,parent_thread_id,
                        root_session_id,agent_role
                 FROM threads ORDER BY thread_id",
            )
            .unwrap();
        let after: Vec<ThreadIdentityRow> = statement
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
        drop(statement);
        assert_eq!(
            after,
            before
                .iter()
                .map(
                    |(thread_id, parent_thread_id, root_session_id, agent_role)| {
                        (
                            thread_id.clone(),
                            "codex".to_owned(),
                            thread_id.clone(),
                            parent_thread_id.clone(),
                            root_session_id.clone(),
                            agent_role.clone(),
                        )
                    }
                )
                .collect::<Vec<_>>()
        );
        assert!(connection
            .execute(
                "INSERT INTO threads(
                    thread_id,source,native_session_id,root_session_id,agent_role,
                    project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('empty-native','codex','', 'empty-native','main','unknown',0,'complete',0)",
                [],
            )
            .is_err());
    }

    #[test]
    fn m02_usage_identity_migration_preserves_tokens_cost_and_quality() {
        let mut connection = v10_connection_with_rows();
        connection
            .execute(
                "UPDATE usage_events
                 SET estimated_cost_nanos_usd=123456789, reasoning_effort='high'
                 WHERE event_id='known'",
                [],
            )
            .unwrap();
        type LegacyUsageSnapshot = (
            i64,
            String,
            Option<i64>,
            i64,
            i64,
            Option<i64>,
            i64,
            i64,
            i64,
            String,
            Option<String>,
        );
        let mut statement = connection
            .prepare(
                "SELECT ledger_epoch,event_id,estimated_cost_nanos_usd,input_tokens,
                        cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,reasoning_effort
                 FROM usage_events ORDER BY event_id",
            )
            .unwrap();
        let before: Vec<LegacyUsageSnapshot> = statement
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
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(before.iter().any(|event| event.2.is_some()));
        drop(statement);
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        type UsageIdentityRow = (
            String,
            i64,
            String,
            Option<i64>,
            i64,
            i64,
            Option<i64>,
            i64,
            i64,
            i64,
            String,
            Option<String>,
        );
        let mut statement = connection
            .prepare(
                "SELECT source,source_epoch,event_id,estimated_cost_nanos_usd,input_tokens,
                        cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,reasoning_effort
                 FROM usage_events ORDER BY event_id",
            )
            .unwrap();
        let after: Vec<UsageIdentityRow> = statement
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
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            after,
            before
                .iter()
                .map(
                    |(
                        ledger_epoch,
                        event_id,
                        estimated_cost_nanos_usd,
                        input_tokens,
                        cached_tokens,
                        cache_write_tokens,
                        output_tokens,
                        reasoning_tokens,
                        total_tokens,
                        quality_status,
                        reasoning_effort,
                    )| {
                        (
                            "codex".to_owned(),
                            *ledger_epoch,
                            event_id.clone(),
                            *estimated_cost_nanos_usd,
                            *input_tokens,
                            *cached_tokens,
                            *cache_write_tokens,
                            *output_tokens,
                            *reasoning_tokens,
                            *total_tokens,
                            quality_status.clone(),
                            reasoning_effort.clone(),
                        )
                    },
                )
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn m03_provenance_lossless_after_canonical_rebuild() {
        let mut connection = v10_connection_with_rows();
        let before: Vec<(i64, i64, i64, i64, i64, String)> = connection
            .prepare(
                "SELECT ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,event_id FROM usage_event_occurrences
                 ORDER BY source_start_offset",
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
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        let after: Vec<(String, i64, i64, i64, i64, i64, String)> = connection
            .prepare(
                "SELECT source,ledger_epoch,source_file_id,file_generation,
                    source_start_offset,source_end_offset,event_id
                 FROM usage_event_occurrences ORDER BY source_start_offset",
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
                    row.get(6)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            after,
            before
                .into_iter()
                .map(|(epoch, file, generation, start, end, event)| {
                    (
                        "codex".to_owned(),
                        epoch,
                        file,
                        generation,
                        start,
                        end,
                        event,
                    )
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn m04_active_build_epoch_is_copied_without_reset() {
        let mut connection = v10_connection_with_rows();
        connection
            .execute(
                "UPDATE app_meta SET usage_active_epoch=5,usage_build_epoch=6,
                    usage_parser_version=7,usage_build_parser_version=7 WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        let epoch: (i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(epoch, (5, Some(6), 7, Some(7)));
    }

    #[test]
    fn m05_revisions_are_unchanged_by_schema_migration() {
        let mut connection = v10_connection_with_rows();
        connection
            .execute(
                "UPDATE app_meta SET data_revision=123,status_revision=456 WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        let revisions: (i64, i64) = connection
            .query_row(
                "SELECT data_revision,status_revision FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(revisions, (123, 456));
    }

    #[test]
    fn app_meta_preservation_migration_copies_all_non_usage_fields() {
        let mut connection = v10_connection_with_rows();
        connection
            .execute(
                "UPDATE app_meta SET
                    metadata_parser_version=17,
                    data_revision=123,
                    status_revision=456,
                    scan_state='failed',
                    active_scan_id=NULL,
                    last_finished_scan_id='finished-scan',
                    last_finished_scan_result='failed',
                    last_scan_started_at_ms=11,
                    last_scan_completed_at_ms=12,
                    last_scan_failed_at_ms=13,
                    last_scan_error_code='SCAN_ERROR',
                    followup_scan_id='followup-scan',
                    followup_state='start_failed',
                    followup_trigger='Manual',
                    followup_requested_at_ms=14,
                    followup_enqueued_status_revision=15,
                    followup_error_code='FOLLOWUP_ERROR',
                    last_full_import_completed_at_ms=16,
                    codex_home_fingerprint='fingerprint',
                    source_binding_status='ready',
                    cost_algorithm_version=7,
                    pricing_catalog_version=8
                 WHERE id=1",
                [],
            )
            .unwrap();
        let snapshot = |connection: &Connection| {
            connection
                .query_row(
                    "SELECT metadata_parser_version,data_revision,status_revision,scan_state,
                        active_scan_id,last_finished_scan_id,last_finished_scan_result,
                        last_scan_started_at_ms,last_scan_completed_at_ms,last_scan_failed_at_ms,
                        last_scan_error_code,followup_scan_id,followup_state,followup_trigger,
                        followup_requested_at_ms,followup_enqueued_status_revision,followup_error_code,
                        last_full_import_completed_at_ms,codex_home_fingerprint,source_binding_status,
                        cost_algorithm_version,pricing_catalog_version
                     FROM app_meta WHERE id=1",
                    [],
                    |row| {
                        let mut values = Vec::with_capacity(22);
                        for index in 0..22 {
                            values.push(row.get::<_, rusqlite::types::Value>(index)?);
                        }
                        Ok(values)
                    },
                )
                .unwrap()
        };
        let before = snapshot(&connection);
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        let after = snapshot(&connection);
        assert_eq!(after, before);
        for removed_column in [
            "usage_active_epoch",
            "usage_build_epoch",
            "usage_parser_version",
            "usage_build_parser_version",
        ] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM pragma_table_info('app_meta') WHERE name=?1",
                        [removed_column],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "removed app_meta column remains: {removed_column}"
            );
        }
    }

    #[test]
    fn m06_migration_does_not_rebuild_or_reset_parser_state() {
        let mut connection = v10_connection_with_rows();
        let before: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT usage_parser_version,canonical_algorithm_version,
                    resolved_through_offset,observed_raw_size FROM usage_source_states",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let counts_before: (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM turns),(SELECT count(*) FROM usage_build_sources)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(migrate(&mut connection, 10).unwrap(), 11);
        let after: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT usage_parser_version,canonical_algorithm_version,
                    resolved_through_offset,observed_raw_size FROM usage_source_states",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let counts_after: (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM turns),(SELECT count(*) FROM usage_build_sources)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(counts_before, counts_after);
    }

    fn damage_table_definition(
        connection: &Connection,
        table: &str,
        needle: &str,
        replacement: &str,
    ) {
        let sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            sql.matches(needle).count(),
            1,
            "migration fixture no longer contains {table} fragment {needle:?}"
        );
        let damaged_sql = sql.replacen(needle, replacement, 1);
        let backup = format!("m07_{table}_backup");
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE {backup} AS SELECT * FROM {table};
                 DROP TABLE {table};"
            ))
            .unwrap();
        connection.execute_batch(&damaged_sql).unwrap();
        connection
            .execute_batch(&format!(
                "INSERT INTO {table} SELECT * FROM {backup};
                 DROP TABLE {backup};"
            ))
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
    }

    fn rebuild_table_without_column(connection: &Connection, table: &str, column: &str) {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info('{table}')"))
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.iter().any(|name| name == column));
        let retained_columns = columns
            .iter()
            .filter(|name| name.as_str() != column)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let backup = format!("m07_{table}_backup");
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE {backup} AS SELECT * FROM {table};
                 DROP TABLE {table};
                 CREATE TABLE {table} AS SELECT {retained_columns} FROM {backup};
                 DROP TABLE {backup};"
            ))
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
    }

    fn assert_m07_rejected(label: &str, damage: impl FnOnce(&Connection)) {
        let (database, connection) = file_v11_connection();
        damage(&connection);
        assert!(
            crate::storage::validate_schema(&connection, database.0.as_path()).is_err(),
            "validate_schema unexpectedly accepted {label}"
        );
        let db_path = database.0.clone();
        let codex_home = db_path.parent().unwrap().join(format!("m07-codex-{label}"));
        drop(connection);
        assert!(
            crate::storage::Ledger::open(crate::storage::LedgerOptions::new(db_path, codex_home,))
                .is_err(),
            "Ledger::open unexpectedly accepted {label}"
        );
    }

    fn assert_m07_damage_rejected(label: &str, table: &str, needle: &str, replacement: &str) {
        assert_m07_rejected(label, |connection| {
            damage_table_definition(connection, table, needle, replacement);
        });
    }

    fn assert_m07_missing_column_rejected(label: &str, table: &str, column: &str) {
        assert_m07_rejected(label, |connection| {
            rebuild_table_without_column(connection, table, column);
        });
    }

    #[test]
    fn m07_schema_bootstrap_validation_covers_v11_contract() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        assert_eq!(migrate(&mut connection, 0).unwrap(), 11);
        crate::storage::validate_schema(&connection, std::path::Path::new(":memory:")).unwrap();
        let source_scan_runs_sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='source_scan_runs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let normalized_sql = source_scan_runs_sql
            .to_ascii_lowercase()
            .replace([' ', '\n', '\t'], "");
        assert!(
            normalized_sql.contains("statein('queued','running','completed','skipped','failed')")
        );
        assert!(normalized_sql.contains("started_at_msisnullorstarted_at_ms>=0"));
        assert!(normalized_sql.contains("finished_at_msisnullorfinished_at_ms>=0"));

        assert_m07_rejected("source_scan_runs missing table", |connection| {
            connection
                .pragma_update(None, "foreign_keys", false)
                .unwrap();
            connection
                .execute("DROP TABLE source_scan_runs", [])
                .unwrap();
        });
        assert_m07_missing_column_rejected(
            "threads native_session_id missing column",
            "threads",
            "native_session_id",
        );
        assert_m07_missing_column_rejected(
            "usage_events source_epoch missing column",
            "usage_events",
            "source_epoch",
        );

        for (label, table, needle, replacement) in [
            (
                "threads native_session_id non-empty check",
                "threads",
                "native_session_id TEXT NOT NULL CHECK (length(native_session_id) > 0)",
                "native_session_id TEXT NOT NULL",
            ),
            (
                "usage_events source NOT NULL",
                "usage_events",
                "source TEXT NOT NULL CHECK (length(source) > 0)",
                "source TEXT CHECK (length(source) > 0)",
            ),
            (
                "usage_events source_epoch NOT NULL",
                "usage_events",
                "source_epoch INTEGER NOT NULL CHECK (source_epoch > 0)",
                "source_epoch INTEGER CHECK (source_epoch > 0)",
            ),
            (
                "usage_events source foreign key",
                "usage_events",
                "FOREIGN KEY (source) REFERENCES source_usage_epochs(source)",
                "CHECK (source IS NOT NULL)",
            ),
            (
                "usage_events primary key",
                "usage_events",
                "PRIMARY KEY (source, source_epoch, event_id)",
                "PRIMARY KEY (source, source_epoch, event_id, occurred_at_ms)",
            ),
            (
                "usage_event_occurrences codex source check",
                "usage_event_occurrences",
                "source TEXT NOT NULL CHECK (source = 'codex')",
                "source TEXT NOT NULL",
            ),
            (
                "source_scan_runs state check",
                "source_scan_runs",
                "state IN ('queued', 'running', 'completed', 'skipped', 'failed')",
                "state IN ('queued')",
            ),
            (
                "source_usage_epochs source non-empty check",
                "source_usage_epochs",
                "source TEXT PRIMARY KEY CHECK (length(source) > 0)",
                "source TEXT PRIMARY KEY",
            ),
            (
                "source_usage_epochs primary key",
                "source_usage_epochs",
                "source TEXT PRIMARY KEY CHECK (length(source) > 0)",
                "source TEXT NOT NULL CHECK (length(source) > 0)",
            ),
        ] {
            assert_m07_damage_rejected(label, table, needle, replacement);
        }
    }

    #[test]
    fn m08_populated_v10_survives_migration_and_production_startup_scan() {
        use crate::{
            ingestion::{IngestionConfig, IngestionCoordinator, LegacyCodexSourceAdapter},
            source::SourceRegistry,
            storage::{Ledger, LedgerOptions},
            usage::{SessionPageRequest, SummaryQuery, TimeRange, UsageFilter, UsageLedger},
        };

        let root = TestRoot::new("spec01-upgrade-startup");
        let codex_home = root.path().join("codex");
        let sessions = codex_home.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(codex_home.join("archived_sessions")).unwrap();

        let rollout_path = sessions.join("rollout-root.jsonl");
        let records = [
            serde_json::json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T01:02:03Z",
                "payload": {
                    "id": "root",
                    "timestamp": "2026-08-08T01:02:03Z",
                    "cwd": "/tmp/usagi-upgrade-startup",
                    "agent_role": "main"
                }
            }),
            serde_json::json!({
                "type": "turn_context",
                "timestamp": "2026-08-08T01:02:04Z",
                "payload": {
                    "turn_id": "00000000-0000-4000-8000-000000000010",
                    "cwd": "/tmp/usagi-upgrade-startup",
                    "model": "gpt"
                }
            }),
            serde_json::json!({
                "type": "event_msg",
                "timestamp": "2026-08-08T01:02:05Z",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 20,
                            "cache_write_input_tokens": 5,
                            "output_tokens": 10,
                            "reasoning_output_tokens": 2,
                            "total_tokens": 110
                        },
                        "last_token_usage": {
                            "input_tokens": 50,
                            "cached_input_tokens": 10,
                            "cache_write_input_tokens": 0,
                            "output_tokens": 5,
                            "reasoning_output_tokens": 1,
                            "total_tokens": 55
                        }
                    }
                }
            }),
        ];
        let mut rollout_bytes = Vec::new();
        for record in records {
            rollout_bytes.extend(serde_json::to_vec(&record).unwrap());
            rollout_bytes.push(b'\n');
        }
        fs::write(&rollout_path, &rollout_bytes).unwrap();

        let state = Connection::open(codex_home.join("state_5.sqlite")).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT NOT NULL,
                    rollout_path TEXT,
                    created_at_ms INTEGER,
                    updated_at_ms INTEGER,
                    archived INTEGER,
                    cwd TEXT,
                    title TEXT,
                    name TEXT,
                    model TEXT,
                    agent_role TEXT
                );
                CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT NOT NULL,
                    child_thread_id TEXT NOT NULL,
                    status TEXT,
                    observed_at_ms INTEGER
                );",
            )
            .unwrap();
        state
            .execute(
                "INSERT INTO threads(
                    id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
                 ) VALUES('root',?1,1700000000000,1700000000100,0,
                          '/tmp/usagi-upgrade-startup','Root',NULL,'gpt','main')",
                [rollout_path.to_str().unwrap()],
            )
            .unwrap();
        drop(state);
        fs::write(
            codex_home.join("session_index.jsonl"),
            b"{\"id\":\"root\",\"thread_name\":\"Root\",\"updated_at\":\"2026-08-08T01:02:05Z\"}\n",
        )
        .unwrap();
        fs::write(codex_home.join(".codex-global-state.json"), b"{}").unwrap();

        let (database, connection) = file_v10_connection_with_rows();
        let rollout_file = fs::File::open(&rollout_path).unwrap();
        let metadata = crate::platform::file_identity::metadata_from_file(&rollout_file).unwrap();
        let (device_id, inode) = metadata.identity.storage_slots().unwrap();
        let observed_size = i64::try_from(metadata.size).unwrap();

        connection
            .execute(
                "UPDATE app_meta
                 SET usage_active_epoch=1,usage_build_epoch=NULL,
                     usage_parser_version=?1,usage_build_parser_version=NULL
                 WHERE id=1",
                [crate::usage::USAGE_PARSER_VERSION],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_files
                 SET current_path=?1,device_id=?2,inode=?3,observed_size=?4,
                     observed_mtime_ns=?5,file_status='present'
                 WHERE source_file_id=1",
                params![
                    rollout_path.to_str().unwrap(),
                    device_id,
                    inode,
                    observed_size,
                    metadata.mtime_ns
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE threads SET current_rollout_path=?1 WHERE thread_id='root'",
                [rollout_path.to_str().unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE usage_source_states
                 SET device_id=?1,inode=?2,usage_parser_version=?3,
                     canonical_algorithm_version=?4,resolved_through_offset=?5,
                     observed_raw_size=?5,raw_tail_status='none',raw_tail_start_offset=NULL
                 WHERE ledger_epoch=1 AND source_file_id=1",
                params![
                    device_id,
                    inode,
                    crate::usage::USAGE_PARSER_VERSION,
                    crate::usage::USAGE_CANONICAL_ALGORITHM_VERSION,
                    observed_size
                ],
            )
            .unwrap();
        drop(connection);

        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(database.0.clone(), codex_home.clone())).unwrap(),
        );
        assert_eq!(ledger.schema_version().unwrap(), 11);

        let range = TimeRange::new(0, i64::MAX).unwrap();
        let usage = UsageLedger::new(&ledger);
        let before = usage.summary(SummaryQuery::new(range, UsageFilter::default())).unwrap();
        assert!(
            before.totals.total_tokens > 0,
            "migrated v10 usage must be visible before startup scan"
        );
        let before_sessions = usage
            .sessions(range, SessionPageRequest::new(10))
            .unwrap();
        assert!(
            before_sessions.rows.iter().any(|row| row.root_session_id == "root"),
            "migrated v10 session must be visible before startup scan"
        );

        let mut registry = SourceRegistry::new();
        registry
            .register(LegacyCodexSourceAdapter::from_home(codex_home.clone()))
            .unwrap();
        let coordinator =
            IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
                .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let scan = ledger.app_state().unwrap().scan;
            if scan.active_scan_id.is_none() && scan.last_finished_scan_id.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for production startup scan"
            );
            thread::sleep(Duration::from_millis(10));
        }
        coordinator.shutdown().unwrap();

        let after = usage.summary(SummaryQuery::new(range, UsageFilter::default())).unwrap();
        assert!(
            after.totals.total_tokens > 0,
            "production startup must not activate an empty Codex dataset"
        );
        let after_sessions = usage
            .sessions(range, SessionPageRequest::new(10))
            .unwrap();
        assert!(
            after_sessions.rows.iter().any(|row| row.root_session_id == "root"),
            "production startup must preserve a visible migrated Codex session"
        );

        let visible_events: i64 = Connection::open(ledger.database_path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*)
                 FROM usage_events ue
                 JOIN source_usage_epochs sue
                   ON sue.source=ue.source AND sue.active_epoch=ue.source_epoch
                 WHERE ue.source='codex'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            visible_events > 0,
            "active Codex epoch must still contain canonical usage after startup"
        );
    }

}
