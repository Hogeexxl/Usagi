use std::{collections::BTreeMap, error::Error, fs, path::Path};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;

#[path = "../../../src/storage/migrations.rs"]
mod production_migrations;

struct MigrationSpec {
    version: u32,
    sql: &'static str,
    foreign_keys_off: bool,
}

const MIGRATIONS: &[MigrationSpec] = &[
    MigrationSpec {
        version: 1,
        sql: include_str!("../../../src/storage/schema/0001_initial.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 2,
        sql: include_str!("../../../src/storage/schema/0002_usage_ledger.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 3,
        sql: include_str!("../../../src/storage/schema/0003_normalized_token_usage.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 4,
        sql: include_str!("../../../src/storage/schema/0004_metadata_parent_v2_cleanup.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 5,
        sql: include_str!("../../../src/storage/schema/0005_project_kind.sql"),
        foreign_keys_off: true,
    },
    MigrationSpec {
        version: 6,
        sql: include_str!("../../../src/storage/schema/0006_subagent_agent_path.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 7,
        sql: include_str!("../../../src/storage/schema/0007_usage_context_and_estimated_cost.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 8,
        sql: include_str!("../../../src/storage/schema/0008_session_resilience.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 9,
        sql: include_str!("../../../src/storage/schema/0009_skill_usage_events.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 10,
        sql: include_str!("../../../src/storage/schema/0010_metadata_fact_ordering.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 11,
        sql: include_str!("../../../src/storage/schema/0011_multi_source_core.sql"),
        foreign_keys_off: true,
    },
    MigrationSpec {
        version: 12,
        sql: include_str!("../../../src/storage/schema/0012_codex_adapter_cutover.sql"),
        foreign_keys_off: true,
    },
    MigrationSpec {
        version: 13,
        sql: include_str!("../../../src/storage/schema/0013_antigravity_adapter_state.sql"),
        foreign_keys_off: false,
    },
    MigrationSpec {
        version: 14,
        sql: include_str!("../../../src/storage/schema/0014_codex_compaction_evidence.sql"),
        foreign_keys_off: false,
    },
];

fn ensure_app_meta_v11_metadata_columns(conn: &Connection) -> rusqlite::Result<()> {
    for (name, definition) in [
        ("metadata_parser_version", "INTEGER NOT NULL DEFAULT 0"),
        ("last_full_import_completed_at_ms", "INTEGER"),
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('app_meta') WHERE name=?1)",
            [name],
            |row| row.get(0),
        )?;
        if !exists {
            conn.execute_batch(&format!(
                "ALTER TABLE app_meta ADD COLUMN {name} {definition};"
            ))?;
        }
    }
    Ok(())
}

fn migrate_to(conn: &mut Connection, target_version: u32) -> rusqlite::Result<()> {
    if !(1..=14).contains(&target_version) {
        return Err(rusqlite::Error::InvalidParameterName(
            "version must be 1–14".into(),
        ));
    }
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    let foreign_keys_off = MIGRATIONS
        .iter()
        .any(|m| m.version <= target_version && m.foreign_keys_off);
    if foreign_keys_off {
        conn.pragma_update(None, "foreign_keys", false)?;
    }
    let result = (|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= target_version) {
            if migration.version == 11 {
                ensure_app_meta_v11_metadata_columns(&tx)?;
            }
            tx.execute_batch(migration.sql)?;
            tx.pragma_update(None, "user_version", migration.version)?;
        }
        if foreign_keys_off {
            let violation = tx.prepare("PRAGMA foreign_key_check")?.exists([])?;
            if violation {
                return Err(rusqlite::Error::InvalidParameterName(
                    "foreign key check failed".into(),
                ));
            }
        }
        tx.commit()
    })();
    let restore = conn.pragma_update(None, "foreign_keys", foreign_keys);
    result?;
    restore
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?.query_map([], |row| row.get(0))?.collect()
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut result: Vec<String> = conn
        .prepare(&format!("PRAGMA table_xinfo({})", quote(table)))?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    result.sort();
    Ok(result)
}

fn check_integrity(conn: &Connection) -> Result<(), Box<dyn Error>> {
    let checks: Vec<String> = conn
        .prepare("PRAGMA quick_check")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if checks != ["ok"] || conn.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err("Oracle database integrity check failed".into());
    }
    Ok(())
}

// Fixed values shared by the historical names of the same fixture relationships.
fn seed_row(
    conn: &Connection,
    table: &str,
    fields: &[(&str, &str)],
    token_groups: &[&str],
    update: Option<&str>,
) -> rusqlite::Result<()> {
    if !tables(conn)?.iter().any(|name| name == table) {
        return Ok(());
    }
    let mut values: BTreeMap<String, String> = [
        ("id", "1"),
        ("source", "'codex'"),
        ("ledger_epoch", "1"),
        ("source_epoch", "1"),
        ("source_file_id", "1"),
        ("file_generation", "1"),
        ("thread_id", "'thread-root'"),
        ("owning_thread_id", "'thread-root'"),
        ("root_session_id", "'thread-root'"),
        ("turn_key", "'turn-1'"),
        ("source_start_offset", "0"),
        ("source_end_offset", "100"),
        ("created_at_ms", "10"),
        ("updated_at_ms", "30"),
    ]
    .iter()
    .chain(fields)
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for group in token_groups {
        for (suffix, value) in [
            ("input_tokens", "10"),
            ("cached_tokens", "2"),
            ("cached_input_tokens", "2"),
            ("cache_write_tokens", "1"),
            ("cache_write_input_tokens", "1"),
            ("output_tokens", "5"),
            ("reasoning_tokens", "1"),
            ("reasoning_output_tokens", "1"),
            ("total_tokens", "15"),
            ("reported_total_tokens", "15"),
            ("derived_total_tokens", "15"),
            ("cache_write_status", "'known'"),
            ("fingerprint", "zeroblob(32)"),
        ] {
            values.insert(format!("{group}_{suffix}"), value.into());
        }
    }
    let present = columns(conn, table)?;
    let fields: Vec<_> = present
        .iter()
        .filter_map(|name| values.get(name).map(|v| (quote(name), v)))
        .collect();
    let sql = if let Some(predicate) = update {
        format!(
            "UPDATE {} SET {} WHERE {predicate}",
            quote(table),
            fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        )
    } else {
        format!(
            "INSERT INTO {}({}) VALUES ({})",
            quote(table),
            fields
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(","),
            fields
                .iter()
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    conn.execute(&sql, []).map(|_| ()).map_err(|err| {
        eprintln!("fixture seed failed for {table}: {err}");
        err
    })
}

fn seed_fixture(conn: &mut Connection, version: u32) -> Result<(), Box<dyn Error>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    seed_row(
        &tx,
        "app_meta",
        &[
            ("data_revision", "7"),
            ("status_revision", "9"),
            ("metadata_parser_version", "3"),
            ("scan_state", "'idle'"),
            ("last_full_import_completed_at_ms", "30"),
            ("codex_home_fingerprint", "'fixture-home'"),
            ("source_binding_status", "'ready'"),
            ("usage_active_epoch", "1"),
            ("usage_build_epoch", "2"),
            ("usage_parser_version", "3"),
            ("usage_build_parser_version", "3"),
            ("cost_algorithm_version", "2"),
            ("pricing_catalog_version", "4"),
        ],
        &[],
        Some("id=1"),
    )?;
    seed_row(
        &tx,
        "source_usage_epochs",
        &[
            ("active_epoch", "1"),
            ("build_epoch", "2"),
            ("active_parser_version", "3"),
            ("build_parser_version", "3"),
        ],
        &[],
        Some("source='codex'"),
    )?;
    seed_row(
        &tx,
        "codex_adapter_state",
        &[
            ("home_fingerprint", "'fixture-home'"),
            ("binding_status", "'ready'"),
        ],
        &[],
        Some("id=1"),
    )?;
    seed_row(
        &tx,
        "threads",
        &[
            ("native_session_id", "'thread-root'"),
            ("agent_role", "'main'"),
            ("title", "'Fixture root'"),
            ("project_name", "'Fixture project'"),
            ("project_path", "'/fixture/project'"),
            ("project_kind", "'project'"),
            ("metadata_model", "'gpt'"),
            ("archived", "0"),
            ("current_rollout_path", "'/fixture/codex-rollout.jsonl'"),
            ("metadata_quality_status", "'complete'"),
            ("metadata_resolved_at_ms", "10"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "scan_runs",
        &[
            ("scan_id", "'scan-fixture'"),
            ("trigger", "'Manual'"),
            ("request_kind", "'direct'"),
            ("state", "'completed'"),
            ("requested_at_ms", "10"),
            ("started_at_ms", "20"),
            ("started_status_revision", "2"),
            ("finished_at_ms", "30"),
            ("terminal_status_revision", "3"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "source_scan_runs",
        &[
            ("scan_id", "'scan-fixture'"),
            ("state", "'completed'"),
            ("started_at_ms", "20"),
            ("finished_at_ms", "30"),
        ],
        &[],
        None,
    )?;
    let private = |name: &str| {
        if version >= 12 {
            format!("codex_{name}")
        } else {
            name.into()
        }
    };
    seed_row(
        &tx,
        &private("source_files"),
        &[
            ("current_path", "'/fixture/codex-rollout.jsonl'"),
            ("source_area", "'sessions'"),
            ("device_id", "1"),
            ("inode", "1"),
            ("observed_size", "100"),
            ("observed_mtime_ns", "1000"),
            ("file_status", "'present'"),
            ("last_seen_at_ms", "30"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("source_checkpoints"),
        &[
            ("consumer_kind", "'usage'"),
            ("parser_version", "3"),
            ("committed_offset", "100"),
            ("guard_hash", "zeroblob(32)"),
            ("processing_status", "'ready'"),
            ("last_successful_scan_at_ms", "30"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("rollout_metadata_facts"),
        &[
            ("metadata_parser_version", "3"),
            ("resolved_through_offset", "100"),
            ("continuation_state", "'owning_live'"),
            ("cwd", "'/fixture/project'"),
            ("cwd_provenance", "'session_meta'"),
            ("cwd_record_offset", "0"),
            ("latest_context_model", "'gpt'"),
            ("latest_context_at_ms", "20"),
            ("agent_role_hint", "'main'"),
            ("agent_role_provenance", "'session_meta_role'"),
            ("agent_role_record_offset", "0"),
            ("agent_path", "'/fixture/agent'"),
            ("agent_path_provenance", "'session_meta'"),
            ("agent_path_record_offset", "0"),
            ("replay_start_offset", "0"),
            ("owning_records_start_offset", "0"),
            ("ownership_confidence", "'confirmed'"),
            ("fact_quality_status", "'complete'"),
            ("latest_context_turn_id", "'turn-1'"),
            ("relationship_conflict", "0"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "usage_events",
        &[
            ("event_id", "'event-1'"),
            ("event_kind", "'normal'"),
            ("occurred_at_ms", "20"),
            ("model", "'gpt'"),
            ("input_tokens", "10"),
            ("cached_input_tokens", "2"),
            ("cached_tokens", "2"),
            ("cache_write_input_tokens", "1"),
            ("cache_write_tokens", "1"),
            ("cache_write_status", "'known'"),
            ("output_tokens", "5"),
            ("reasoning_output_tokens", "1"),
            ("reasoning_tokens", "1"),
            ("total_tokens", "15"),
            ("quality_status", "'complete'"),
            ("reasoning_effort", "'medium'"),
            ("estimated_cost_nanos_usd", "42"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("usage_event_occurrences"),
        &[("event_id", "'event-1'")],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "codex_usage_event_facts",
        &[
            ("event_id", "'event-1'"),
            ("response_id", "'response-1'"),
            ("evidence_kind", "'explicit'"),
            ("operation", "'compaction'"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "codex_compaction_markers",
        &[
            ("occurred_at_ms", "20"),
            ("model", "'gpt'"),
            ("response_id", "'response-1'"),
            ("resolved_event_id", "'event-1'"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "codex_usage_reconciliation_windows",
        &[("state_json", "'{}'")],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "codex_usage_event_holds",
        &[("event_id", "'event-1'"), ("hold_reason", "'replay'")],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("turns"),
        &[
            ("raw_turn_id", "'turn-1'"),
            ("started_at_ms", "10"),
            ("ended_at_ms", "30"),
            ("start_offset", "0"),
            ("end_offset", "100"),
            ("status", "'completed'"),
            ("accounted_candidate_count", "1"),
            ("model_state", "'single'"),
            ("single_model", "'gpt'"),
            ("unresolved_model_seen", "0"),
            ("reasoning_effort_state", "'none'"),
            ("unresolved_reasoning_effort_seen", "0"),
            ("compensation_allowed", "1"),
            ("block_start_missing", "0"),
            ("block_time_missing", "0"),
            ("block_reset", "0"),
            ("block_ownership_gap", "0"),
            ("block_parser_gap", "0"),
            ("block_required_invalid", "0"),
            ("block_model_unresolved", "0"),
            ("quality_status", "'complete'"),
            ("state_through_offset", "100"),
        ],
        &["start_total", "last_total", "accounted"],
        None,
    )?;
    seed_row(
        &tx,
        &private("ingest_anomalies"),
        &[
            ("anomaly_id", "'anomaly-1'"),
            ("detected_at_ms", "30"),
            ("occurred_at_ms", "20"),
            ("anomaly_type", "'FIXTURE'"),
            ("severity", "'warning'"),
            ("details_json", "'{}'"),
            ("resolved", "0"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("usage_source_states"),
        &[
            ("device_id", "1"),
            ("inode", "1"),
            ("usage_parser_version", "3"),
            ("canonical_algorithm_version", "2"),
            ("resolved_through_offset", "100"),
            ("observed_raw_size", "100"),
            ("raw_tail_status", "'none'"),
            ("continuation_state", "'owning_live'"),
            ("previous_total_offset", "100"),
            ("chain_state", "'continuous'"),
            ("active_turn_key", "'turn-1'"),
            ("active_model", "'gpt'"),
            ("active_model_offset", "20"),
            ("active_reasoning_effort", "'medium'"),
            ("active_reasoning_effort_offset", "20"),
        ],
        &["previous_total"],
        None,
    )?;
    seed_row(
        &tx,
        &private("usage_build_sources"),
        &[
            ("build_epoch", "2"),
            ("target_parser_version", "3"),
            ("expected_file_generation", "1"),
            ("expected_device_id", "1"),
            ("expected_inode", "1"),
            ("expected_owning_thread_id", "'thread-root'"),
            ("expected_root_session_id", "'thread-root'"),
            ("active_committed_offset", "100"),
            ("active_guard_hash", "zeroblob(32)"),
            ("active_state_fingerprint", "zeroblob(32)"),
            ("required_generation", "1"),
            ("required_through_offset", "100"),
            ("observed_raw_size", "100"),
            ("raw_tail_status", "'none'"),
            ("membership_reason", "'active_contributor'"),
            ("completion_status", "'rebuilt'"),
            ("completed_generation", "1"),
            ("completed_through_offset", "100"),
            ("carry_phase", "'none'"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("usage_session_quarantine"),
        &[
            ("primary_error_code", "'FIXTURE_ERROR'"),
            ("last_activity_at_ms", "30"),
            ("first_seen_at_ms", "10"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("usage_session_quarantine_sources"),
        &[("device_id", "1"), ("inode", "1"), ("observed_size", "100")],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        &private("skill_usage_events"),
        &[
            ("occurred_at_ms", "20"),
            ("model", "'gpt'"),
            ("skill_name", "'fixture-skill'"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "antigravity_conversation_state",
        &[
            ("conversation_id", "'conversation-fixture'"),
            ("observed_gen_max_idx", "2"),
            ("observed_step_max_idx", "3"),
            ("last_scanned_at_ms", "30"),
        ],
        &[],
        None,
    )?;
    seed_row(
        &tx,
        "antigravity_usage_quarantine",
        &[
            ("conversation_id", "'conversation-fixture'"),
            (
                "payload_digest",
                "'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
            ),
            ("gen_idx", "2"),
            ("reason_code", "'USAGE_TOKEN_INVALID'"),
            ("first_seen_at_ms", "20"),
            ("last_seen_at_ms", "30"),
        ],
        &[],
        None,
    )?;
    for table in tables(&tx)? {
        let count: i64 = tx.query_row(
            &format!("SELECT count(*) FROM {}", quote(&table)),
            [],
            |row| row.get(0),
        )?;
        if count == 0 {
            return Err(format!("fixture table {table} has no rows at v{version}").into());
        }
    }
    tx.commit()?;
    check_integrity(conn)
}

#[derive(Serialize)]
struct Profile {
    version: u32,
    tables: BTreeMap<String, Vec<String>>,
}

fn required<'a>(args: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, Box<dyn Error>> {
    args.get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{name}").into())
}

fn version(args: &BTreeMap<String, String>, key: &str) -> Result<u32, Box<dyn Error>> {
    let version: u32 = required(args, key)?.parse()?;
    if !(1..=14).contains(&version) {
        return Err("version must be 1–14".into());
    }
    Ok(version)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut cli = std::env::args().skip(1);
    let command = cli
        .next()
        .ok_or("expected create-input, create-expected, or profile")?;
    let allowed: &[&str] = match command.as_str() {
        "create-input" => &["version", "output"],
        "create-expected" => &["input", "from-version", "output"],
        "profile" => &["db", "output"],
        _ => return Err(format!("unknown subcommand {command}").into()),
    };
    let mut args = BTreeMap::new();
    while let Some(option) = cli.next() {
        let key = option.strip_prefix("--").ok_or("expected --option value")?;
        if !allowed.contains(&key)
            || args
                .insert(key.to_owned(), cli.next().ok_or("missing option value")?)
                .is_some()
        {
            return Err(format!("unknown or duplicate option {option}").into());
        }
    }
    let output = Path::new(required(&args, "output")?);
    if output.exists() {
        return Err(format!("output already exists: {}", output.display()).into());
    }
    match command.as_str() {
        "create-input" => {
            let version = version(&args, "version")?;
            let mut conn = Connection::open(output)?;
            conn.pragma_update(None, "foreign_keys", true)?;
            migrate_to(&mut conn, version)?;
            seed_fixture(&mut conn, version)?;
            println!(
                "create-input v{version}: {} tables; integrity=ok",
                tables(&conn)?.len()
            );
        }
        "create-expected" => {
            let from_version = version(&args, "from-version")?;
            fs::copy(required(&args, "input")?, output)?;
            let mut conn = Connection::open(output)?;
            let actual: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if actual != from_version {
                return Err(
                    format!("input user_version {actual} differs from {from_version}").into(),
                );
            }
            conn.pragma_update(None, "foreign_keys", true)?;
            let final_version = production_migrations::migrate(&mut conn, from_version)?;
            if final_version != production_migrations::latest_schema_version() {
                return Err("production migration did not reach v14".into());
            }
            check_integrity(&conn)?;
            println!("create-expected v{from_version}→v{final_version}: integrity=ok");
        }
        "profile" => {
            let conn = Connection::open_with_flags(
                required(&args, "db")?,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let version = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            let mut profile_tables = BTreeMap::new();
            for table in tables(&conn)? {
                profile_tables.insert(table.clone(), columns(&conn, &table)?);
            }
            let profile = Profile {
                version,
                tables: profile_tables,
            };
            fs::write(
                output,
                format!("{}\n", serde_json::to_string_pretty(&profile)?),
            )?;
            println!("profile v{version}: {} tables", profile.tables.len());
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("rust-schema-oracle: {err}");
        std::process::exit(1);
    }
}
