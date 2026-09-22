//! Track D API and Public v1 regression tests.
//!
//! Enforces:
//! - [TD-P4-HEALTH-01]: Root union, overlap error precedence, deduplicated internal Cost incomplete,
//!   and canonical public paged sessions / totals.
//! - [TD-P4-SIDECAR-OR-01]: Codex error sidecar OR predicate with various source filters.
//! - [TD-P4-TERMINAL-01]: Terminal filter options authority (registered sources only, sorted Codex -> Antigravity).
//! - [TD-P4-FILTER-RUST-01]: Public Rust filter_options_snapshot retains DB sources while HTTP facade isolates them.
//! - [TD-P4-MODEL-GROUP-01]: Model display groups: Antigravity provider vs RouteModels.
//! - [TD-P4-MAIN-MODEL-01]: Main-only session model semantics (subagent models excluded from session row/sort/filter).
//! - [TD-P4-DETAIL-01]: Project carrier fields present in response.main and absent from top-level.
//! - [TD-P4-PUBLIC-01]: Public revision over-invalidation with Codex totals isolation.
//! - [TD-P4-PUBLIC-HEALTH-01]: Public API v1 legacy health projection truth table and checked arithmetic boundaries.
//! - [TD-P4-PUBLIC-STATUS-01]: Public /api/v1/status isolates Codex child run even when parent scan failed.
//! - [TD-P4-QUOTA-01]: Codex quota endpoints contract stability with mixed-source data.
//! - Migrated assertions from old mixed-source test with registered terminal options enforcement.

use super::*;
use axum::http::{Method, StatusCode};
use rusqlite::Connection;

use crate::{
    codex::CodexSessionErrorSidecar,
    source::SourceId,
    usage::{
        TimeRange, UsageFilter,
        aggregate::{
            AggregateReader, LegacyPublicUsageProjection, SessionErrorSidecar, UsageSummary,
        },
        ledger::UsageLedger,
    },
};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn t_track_d_health_root_union_and_overlap_precedence() {
    let fixture = support::ApiFixture::track_d("health-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();

        // 1. Overlap root: has canonical usage event with unknown cost, AND is in error sidecar
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('root-overlap', 'codex', 'native-overlap', NULL, 'root-overlap',
                       'main', 'Overlap Root', 'Proj', '/path', 'project', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-overlap', 'normal', ?1,
                       'root-overlap', 'root-overlap', NULL, 'gpt-4o', NULL,
                       NULL, 100, 20, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO codex_usage_session_quarantine(
                ledger_epoch, root_session_id, primary_error_code, last_activity_at_ms, first_seen_at_ms, updated_at_ms
             ) VALUES (1, 'root-overlap', 'QUARANTINED_OVERLAP', ?1, ?1, ?1)",
            [ts],
        )
        .unwrap();

        // 2. Error-only root: in error sidecar, but has NO canonical usage events
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('root-error-only', 'codex', 'native-error', NULL, 'root-error-only',
                       'main', 'Error Only Root', 'Proj', '/path', 'project', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO codex_usage_session_quarantine(
                ledger_epoch, root_session_id, primary_error_code, last_activity_at_ms, first_seen_at_ms, updated_at_ms
             ) VALUES (1, 'root-error-only', 'QUARANTINED_ONLY', ?1, ?1, ?1)",
            [ts],
        )
        .unwrap();

        // 3. Cost-complete canonical root: has complete cost, NOT in error sidecar
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('root-complete', 'codex', 'native-complete', NULL, 'root-complete',
                       'main', 'Complete Root', 'Proj', '/path', 'project', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-complete', 'normal', ?1,
                       'root-complete', 'root-complete', NULL, 'gpt-4o', NULL,
                       20_000_000, 300, 50, 0, 200, 0, 500, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // A. HTTP Internal Session list: root union without duplicates, overlap has error precedence
    let sessions_resp = fixture
        .call(Method::GET, "/api/usage/sessions?range=year", &[])
        .await;
    assert_eq!(sessions_resp.status(), StatusCode::OK);
    let sessions = json_body(sessions_resp).await;
    let items = sessions["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "HTTP items should have union cardinality 3");

    let sort_index = sessions["sort_index"].as_array().unwrap();
    assert_eq!(
        sort_index.len(),
        3,
        "sort_index should have union cardinality 3"
    );

    let overlap_item = items
        .iter()
        .find(|s| s["root_session_id"] == "root-overlap")
        .expect("root-overlap item");
    assert_eq!(
        overlap_item["data_status"], "error",
        "overlap item takes error status"
    );
    assert_eq!(overlap_item["error_code"], "QUARANTINED_OVERLAP");

    let overlap_index = sort_index
        .iter()
        .find(|s| s["root_session_id"] == "root-overlap")
        .expect("root-overlap sort index");
    assert_eq!(overlap_index["data_status"], "error");

    let complete_item = items
        .iter()
        .find(|s| s["root_session_id"] == "root-complete")
        .expect("root-complete item");
    assert_eq!(complete_item["data_status"], "complete");

    // B. Public paged UsageLedger::sessions() and verify_invariants remain canonical-only (2 roots)
    let sidecars: [&dyn SessionErrorSidecar; 1] = [&CodexSessionErrorSidecar];
    let usage_ledger = UsageLedger::new(&fixture.ledger, &sidecars);
    let range = TimeRange::new(0, ts + 100_000).unwrap();
    let paged = usage_ledger
        .sessions(range, crate::usage::SessionPageRequest::new(10))
        .unwrap();
    assert_eq!(
        paged.rows.len(),
        2,
        "public paged sessions sees only 2 canonical roots"
    );
    assert!(
        paged
            .rows
            .iter()
            .any(|r| r.root_session_id == "root-overlap")
    );
    assert!(
        paged
            .rows
            .iter()
            .any(|r| r.root_session_id == "root-complete")
    );
    assert!(
        !paged
            .rows
            .iter()
            .any(|r| r.root_session_id == "root-error-only")
    );

    // Verify invariants pass on canonical authority
    fixture
        .ledger
        .with_read_transaction(|tx| -> Result<(), crate::storage::StorageError> {
            let reader = AggregateReader::new(tx, &sidecars);
            assert!(reader.verify_invariants(range).is_ok());
            Ok(())
        })
        .unwrap();

    // C. Internal summary metrics
    let summary_resp = fixture
        .call(Method::GET, "/api/usage/summary?range=year", &[])
        .await;
    assert_eq!(summary_resp.status(), StatusCode::OK);
    let summary = json_body(summary_resp).await;

    // Totals include canonical usage of both canonical roots (200 + 500 = 700 tokens)
    assert_eq!(summary["usage"]["total_tokens"], 700);
    // session_count is count of distinct canonical roots (2)
    assert_eq!(summary["usage"]["session_count"], 2);
    // cost_incomplete_session_count deduplicates overlap: root-overlap (1) + root-error-only (1) = 2
    assert_eq!(summary["usage"]["cost_incomplete_session_count"], 2);
    // health counts: complete=1 (only root-complete), incomplete=0, error=2, total=3
    assert_eq!(summary["usage"]["session_health"]["complete_sessions"], 1);
    assert_eq!(summary["usage"]["session_health"]["incomplete_sessions"], 0);
    assert_eq!(summary["usage"]["session_health"]["error_sessions"], 2);
    assert_eq!(summary["usage"]["session_health"]["total_sessions"], 3);

    fixture.scanner.shutdown().unwrap();
}

#[test]
fn t_td_p4_sidecar_or_01_filter_predicates() {
    let sidecar = CodexSessionErrorSidecar;
    let range = TimeRange::new(0, 10_000).unwrap();

    let temp_dir = std::env::temp_dir().join(format!("test-sidecar-or-{}", now_ms()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let conn = Connection::open(temp_dir.join("test.db")).unwrap();

    conn.execute(
        "CREATE TABLE source_usage_epochs (
            source TEXT PRIMARY KEY,
            active_epoch INTEGER NOT NULL,
            active_parser_version INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO source_usage_epochs VALUES ('codex', 1, 1)", [])
        .unwrap();

    conn.execute(
        "CREATE TABLE threads (
            thread_id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            native_session_id TEXT,
            parent_thread_id TEXT,
            root_session_id TEXT NOT NULL,
            agent_role TEXT NOT NULL,
            title TEXT,
            project_name TEXT,
            project_path TEXT,
            project_kind TEXT NOT NULL,
            metadata_model TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            archived INTEGER NOT NULL,
            metadata_quality_status TEXT NOT NULL,
            metadata_resolved_at_ms INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "CREATE TABLE codex_usage_session_quarantine (
            ledger_epoch INTEGER NOT NULL,
            root_session_id TEXT NOT NULL,
            primary_error_code TEXT NOT NULL,
            last_activity_at_ms INTEGER NOT NULL,
            first_seen_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (ledger_epoch, root_session_id)
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO threads VALUES (
            'root-codex-err', 'codex', 'native-1', NULL, 'root-codex-err',
            'main', 'Title', 'Proj', '/path', 'project', NULL,
            5000, 5000, 0, 'complete', 5000
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO codex_usage_session_quarantine VALUES (
            1, 'root-codex-err', 'ERR_CODE', 5000, 5000, 5000
        )",
        [],
    )
    .unwrap();

    // 1. sources: [] -> includes Codex
    let roots_empty = sidecar
        .error_roots(&conn, range, &UsageFilter::default())
        .unwrap();
    assert_eq!(roots_empty.len(), 1);
    assert_eq!(roots_empty[0].root_session_id, "root-codex-err");

    // 2. sources: [codex] -> includes Codex
    let filter_codex = UsageFilter::default().with_sources(vec![SourceId::CODEX]);
    let roots_codex = sidecar.error_roots(&conn, range, &filter_codex).unwrap();
    assert_eq!(roots_codex.len(), 1);

    // 3. sources: [codex, antigravity] -> includes Codex
    let filter_both =
        UsageFilter::default().with_sources(vec![SourceId::CODEX, SourceId::ANTIGRAVITY]);
    let roots_both = sidecar.error_roots(&conn, range, &filter_both).unwrap();
    assert_eq!(roots_both.len(), 1);

    // 4. sources: [antigravity] -> excludes Codex, returns empty
    let filter_ag = UsageFilter::default().with_sources(vec![SourceId::ANTIGRAVITY]);
    let roots_ag = sidecar.error_roots(&conn, range, &filter_ag).unwrap();
    assert_eq!(roots_ag.len(), 0);

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn t_td_p4_terminal_01_registered_sources_authority() {
    let fixture = support::ApiFixture::track_d("terminal-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        // Insert usage for unregistered fake-source
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('fake-source', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('fake-root', 'fake-source', 'native-fake', NULL, 'fake-root',
                       'main', 'Fake Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('fake-source', 1, 'fake-ev', 'normal', ?1,
                       'fake-root', 'fake-root', NULL, 'fake-model', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    let resp = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let sources = body["sources"].as_array().unwrap();

    // Must strictly be registered sources in order: Codex (10) -> Antigravity (20)
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["source"], "codex");
    assert_eq!(sources[0]["display_name"], "Codex");
    assert_eq!(sources[1]["source"], "antigravity");
    assert_eq!(sources[1]["display_name"], "Antigravity");

    // fake-source MUST NOT appear
    assert!(!sources.iter().any(|s| s["source"] == "fake-source"));

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_filter_rust_01_facade_isolates_db_sources() {
    let fixture = support::ApiFixture::track_d("filter-rust-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('fake-source', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('fake-root', 'fake-source', 'native-fake', NULL, 'fake-root',
                       'main', 'Fake Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('fake-source', 1, 'fake-ev', 'normal', ?1,
                       'fake-root', 'fake-root', NULL, 'fake-model', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // Public Rust UsageLedger::filter_options_snapshot returns distinct DB sources
    let sidecars: [&dyn SessionErrorSidecar; 1] = [&CodexSessionErrorSidecar];
    let usage_ledger = UsageLedger::new(&fixture.ledger, &sidecars);
    let snapshot = usage_ledger.filter_options_snapshot().unwrap();
    assert!(
        snapshot
            .value
            .sources
            .iter()
            .any(|s| s.source == "fake-source"),
        "Public Rust method preserves DB distinct sources"
    );

    // HTTP facade registered_filter_options_response returns ONLY registry sources
    let resp = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    let body = json_body(resp).await;
    let sources = body["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["source"], "codex");
    assert_eq!(sources[1]["source"], "antigravity");
    assert!(!sources.iter().any(|s| s["source"] == "fake-source"));

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_track_d_model_semantics_model_group_antigravity_and_route_models() {
    let fixture = support::ApiFixture::track_d("model-group-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('antigravity', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();

        // Antigravity event with custom model
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('ag-root', 'antigravity', 'ag-native', NULL, 'ag-root',
                       'main', 'AG Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('antigravity', 1, 'ag-ev', 'normal', ?1,
                       'ag-root', 'ag-root', NULL, 'gemini-2.5-pro', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();

        // Codex event with route model
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('codex-root', 'codex', 'cx-native', NULL, 'codex-root',
                       'main', 'CX Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'cx-ev', 'normal', ?1,
                       'codex-root', 'codex-root', NULL, 'claude-3-5-sonnet', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    let resp = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let models = body["models"].as_array().unwrap();

    let ag_model = models
        .iter()
        .find(|m| m["model"] == "gemini-2.5-pro")
        .expect("gemini model");
    assert_eq!(ag_model["provider"], "antigravity");

    let route_model = models
        .iter()
        .find(|m| m["model"] == "claude-3-5-sonnet")
        .expect("claude model");
    assert_eq!(route_model["provider"], "route-models");

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_track_d_model_semantics_main_model_session_main_only() {
    let fixture = support::ApiFixture::track_d("main-model-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('main-root', 'codex', 'native-main', NULL, 'main-root',
                       'main', 'Main Thread', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('sub-thread', 'codex', 'native-sub', 'main-root', 'main-root',
                       'subagent', 'Sub Thread', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();

        // Main usage event uses model-main
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-main', 'normal', ?1,
                       'main-root', 'main-root', NULL, 'model-main', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();

        // Subagent usage event uses model-subagent-only
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-sub', 'normal', ?1,
                       'sub-thread', 'main-root', NULL, 'model-subagent-only', NULL,
                       NULL, 50, 0, 0, 50, 0, 100, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // Global filter options include subagent model
    let options_resp = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    let options = json_body(options_resp).await;
    let models = options["models"].as_array().unwrap();
    assert!(models.iter().any(|m| m["model"] == "model-main"));
    assert!(models.iter().any(|m| m["model"] == "model-subagent-only"));

    // Session row models_used only carries model-main
    let sessions_resp = fixture
        .call(Method::GET, "/api/usage/sessions?range=year", &[])
        .await;
    let sessions = json_body(sessions_resp).await;
    let item = &sessions["items"].as_array().unwrap()[0];
    assert_eq!(item["models_used"], serde_json::json!(["model-main"]));

    // Filtering by subagent-only model returns 0 sessions
    let filtered_resp = fixture
        .call(
            Method::GET,
            "/api/usage/sessions?range=year&model=model-subagent-only",
            &[],
        )
        .await;
    let filtered = json_body(filtered_resp).await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 0);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_detail_01_project_carrier_in_main_dto() {
    let fixture = support::ApiFixture::track_d("detail-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();

        // Root with project
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('root-with-proj', 'codex', 'native-with-proj', NULL, 'root-with-proj',
                       'main', 'Proj Title', 'Project Alpha', '/src/alpha', 'project', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-with-proj', 'normal', ?1,
                       'root-with-proj', 'root-with-proj', NULL, 'gpt-4o', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();

        // Root without project
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('root-no-proj', 'codex', 'native-no-proj', NULL, 'root-no-proj',
                       'main', 'No Proj Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-no-proj', 'normal', ?1,
                       'root-no-proj', 'root-no-proj', NULL, 'gpt-4o', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // Detail with project
    let resp1 = fixture
        .call(
            Method::GET,
            "/api/usage/sessions/root-with-proj/detail?range=year",
            &[],
        )
        .await;
    assert_eq!(resp1.status(), StatusCode::OK);
    let detail1 = json_body(resp1).await;
    assert_eq!(detail1["main"]["project_name"], "Project Alpha");
    assert_eq!(detail1["main"]["project_path"], "/src/alpha");
    assert!(
        detail1.get("project_name").is_none(),
        "top-level must not have project_name"
    );
    assert!(
        detail1.get("project_path").is_none(),
        "top-level must not have project_path"
    );

    // Detail without project
    let resp2 = fixture
        .call(
            Method::GET,
            "/api/usage/sessions/root-no-proj/detail?range=year",
            &[],
        )
        .await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let detail2 = json_body(resp2).await;
    assert_eq!(detail2["main"]["project_name"], serde_json::Value::Null);
    assert_eq!(detail2["main"]["project_path"], serde_json::Value::Null);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_public_01_revision_bump_and_codex_isolation() {
    let fixture = support::ApiFixture::track_d("public-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('antigravity', 1, 1)",
            [],
        )
        .unwrap();

        // Codex event
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('cx-root', 'codex', 'cx-native', NULL, 'cx-root',
                       'main', 'CX Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'cx-ev', 'normal', ?1,
                       'cx-root', 'cx-root', NULL, 'gpt-4o', NULL,
                       10_000_000, 200, 0, 0, 100, 0, 300, 'complete', ?1)",
            [ts],
        )
        .unwrap();

        // Antigravity event
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('ag-root', 'antigravity', 'ag-native', NULL, 'ag-root',
                       'main', 'AG Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('antigravity', 1, 'ag-ev', 'normal', ?1,
                       'ag-root', 'ag-root', NULL, 'gemini-2.5-pro', NULL,
                       NULL, 500, 0, 0, 500, 0, 1000, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // Public summary before bump: only Codex data (300 tokens)
    let resp1 = fixture
        .call(Method::GET, "/api/v1/usage/summary?range=year", &[])
        .await;
    assert_eq!(resp1.status(), StatusCode::OK);
    let summary1 = json_body(resp1).await;
    assert_eq!(summary1["usage"]["total_tokens"], 300);
    assert_eq!(summary1["usage"]["session_count"], 1);
    let rev1 = summary1["data_revision"].as_i64().unwrap();

    // Bump global data_revision as Antigravity scan would do
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE app_meta SET data_revision = data_revision + 5 WHERE id = 1",
            [],
        )
        .unwrap();
    }

    // Public summary after bump: returns new global data_revision, but Codex-only totals identical
    let resp2 = fixture
        .call(Method::GET, "/api/v1/usage/summary?range=year", &[])
        .await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let summary2 = json_body(resp2).await;
    assert_eq!(summary2["data_revision"], rev1 + 5);
    assert_eq!(summary2["usage"]["total_tokens"], 300);
    assert_eq!(summary2["usage"]["session_count"], 1);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_public_health_01_legacy_projection_boundaries() {
    let fixture = support::ApiFixture::track_d("public-health-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();

        // 1. Root with unknown cost AND in sidecar error
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('codex-overlap', 'codex', 'native-overlap', NULL, 'codex-overlap',
                       'main', 'Overlap', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-overlap', 'normal', ?1,
                       'codex-overlap', 'codex-overlap', NULL, 'gpt-4o', NULL,
                       NULL, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO codex_usage_session_quarantine(
                ledger_epoch, root_session_id, primary_error_code, last_activity_at_ms, first_seen_at_ms, updated_at_ms
             ) VALUES (1, 'codex-overlap', 'ERR', ?1, ?1, ?1)",
            [ts],
        )
        .unwrap();

        // 2. Root with complete cost
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('codex-complete', 'codex', 'native-comp', NULL, 'codex-complete',
                       'main', 'Complete', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('codex', 1, 'ev-comp', 'normal', ?1,
                       'codex-complete', 'codex-complete', NULL, 'gpt-4o', NULL,
                       10_000_000, 100, 0, 0, 100, 0, 200, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // Public API v1 /api/v1/usage/summary
    let pub_resp = fixture
        .call(Method::GET, "/api/v1/usage/summary?range=year", &[])
        .await;
    assert_eq!(pub_resp.status(), StatusCode::OK);
    let pub_body = json_body(pub_resp).await;
    let pub_usage = &pub_body["usage"];

    // Legacy public semantics:
    // incomplete_sessions = canonical_cost_incomplete (1)
    // error_sessions = sidecar_error (1)
    // cost_incomplete_session_count = canonical_cost_incomplete + sidecar_error = 1 + 1 = 2 (NOT union-collapsed!)
    // complete_sessions = canonical_session_count - canonical_cost_incomplete = 2 - 1 = 1
    // total_sessions = canonical_session_count + sidecar_error = 2 + 1 = 3
    assert_eq!(pub_usage["session_health"]["incomplete_sessions"], 1);
    assert_eq!(pub_usage["session_health"]["error_sessions"], 1);
    assert_eq!(pub_usage["session_health"]["complete_sessions"], 1);
    assert_eq!(pub_usage["session_health"]["total_sessions"], 3);
    assert_eq!(pub_usage["cost_incomplete_session_count"], 2);
    assert_eq!(pub_usage["session_count"], 2);

    // Internal Dashboard API retains union health:
    let int_resp = fixture
        .call(Method::GET, "/api/usage/summary?range=year", &[])
        .await;
    assert_eq!(int_resp.status(), StatusCode::OK);
    let int_body = json_body(int_resp).await;
    let int_usage = &int_body["usage"];
    assert_eq!(int_usage["session_health"]["incomplete_sessions"], 0);
    assert_eq!(int_usage["session_health"]["error_sessions"], 1);
    assert_eq!(int_usage["session_health"]["complete_sessions"], 1);
    assert_eq!(int_usage["session_health"]["total_sessions"], 2);
    assert_eq!(int_usage["cost_incomplete_session_count"], 1);

    // Test projection helper boundaries directly
    let dummy_summary = UsageSummary {
        totals: crate::usage::TokenTotals {
            input_tokens: 0,
            cached_tokens: 0,
            cache_write_tokens: None,
            output_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            uncached_input_tokens: None,
            other_output_tokens: 0,
            cache_hit_rate: None,
            estimated_cost_nanos_usd: None,
            cost_completeness: crate::usage::aggregate::CostCompleteness::Empty,
        },
        session_count: 0,
        cost_incomplete_session_count: 0,
        complete_session_cost_per_million_tokens: None,
        health: crate::usage::aggregate::SessionHealthSummary {
            total_sessions: 0,
            complete_sessions: 0,
            incomplete_sessions: 0,
            error_sessions: 0,
        },
    };

    // 1. Underflow: canonical_session_count < canonical_cost_incomplete_root_count
    let bad_legacy_1 = LegacyPublicUsageProjection {
        canonical_session_count: 1,
        canonical_cost_incomplete_root_count: 5,
        sidecar_error_root_count: 0,
        canonical_complete_session_cost_per_million_tokens: None,
    };
    assert!(
        crate::api::public_v1::apply_legacy_public_projection(dummy_summary.clone(), bad_legacy_1)
            .is_err(),
        "underflow must be rejected"
    );

    // 2. Overflow: value > JSON_SAFE_INTEGER_MAX
    let bad_legacy_2 = LegacyPublicUsageProjection {
        canonical_session_count: (1_i64 << 53) + 10,
        canonical_cost_incomplete_root_count: 0,
        sidecar_error_root_count: 0,
        canonical_complete_session_cost_per_million_tokens: None,
    };
    assert!(
        crate::api::public_v1::apply_legacy_public_projection(dummy_summary.clone(), bad_legacy_2)
            .is_err(),
        "unsafe integer must be rejected"
    );

    // 3. Addition overflow
    let bad_legacy_3 = LegacyPublicUsageProjection {
        canonical_session_count: i64::MAX,
        canonical_cost_incomplete_root_count: 0,
        sidecar_error_root_count: 1,
        canonical_complete_session_cost_per_million_tokens: None,
    };
    assert!(
        crate::api::public_v1::apply_legacy_public_projection(dummy_summary, bad_legacy_3).is_err(),
        "addition overflow must be rejected"
    );

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_public_status_01_codex_child_isolation() {
    let fixture = support::ApiFixture::track_d("public-status-01");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DELETE FROM source_scan_runs", []).unwrap();
        conn.execute("DELETE FROM scan_runs", []).unwrap();

        // Insert parent scan run: failed
        conn.execute(
            "INSERT INTO scan_runs(
                scan_id, trigger, request_kind, state, requested_at_ms,
                started_at_ms, started_status_revision, finished_at_ms, terminal_status_revision, error_code
             ) VALUES ('scan-mixed-1', 'Manual', 'direct', 'failed', ?1,
                       ?1, 1, ?1 + 1000, 2, 'SOURCE_RUN_FAILED')",
            [ts],
        )
        .unwrap();

        // Insert child scan run: Codex completed
        conn.execute(
            "INSERT INTO source_scan_runs(
                scan_id, source, state, started_at_ms, finished_at_ms, error_code
             ) VALUES ('scan-mixed-1', 'codex', 'completed', ?1, ?1 + 800, NULL)",
            [ts],
        )
        .unwrap();

        // Insert child scan run: Antigravity failed
        conn.execute(
            "INSERT INTO source_scan_runs(
                scan_id, source, state, started_at_ms, finished_at_ms, error_code
             ) VALUES ('scan-mixed-1', 'antigravity', 'failed', ?1, ?1 + 1000, 'ANTIGRAVITY_IO')",
            [ts],
        )
        .unwrap();
    }

    let resp = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;

    // Public status reflects Codex child only: completed, no error code, finished timestamp = ts + 800
    assert_eq!(body["last_finished_scan_result"], "completed");
    assert_eq!(body["last_scan_error_code"], serde_json::Value::Null);
    assert_eq!(body["last_scan_completed_at_ms"], ts + 800);
    assert_eq!(body["last_scan_failed_at_ms"], serde_json::Value::Null);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_quota_01_quota_endpoint_contract() {
    let fixture = support::ApiFixture::track_d("quota-01");
    let db_path = fixture._root.path().join("mu.sqlite3");

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('codex', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('antigravity', 1, 1)",
            [],
        )
        .unwrap();
    }

    // Public /api/v1/codex/quota
    let resp1 = fixture.call(Method::GET, "/api/v1/codex/quota", &[]).await;
    assert_eq!(resp1.status(), StatusCode::OK);
    let body1 = json_body(resp1).await;
    assert!(body1.get("status").is_some());

    // Internal /api/codex/quota
    let resp2 = fixture.call(Method::GET, "/api/codex/quota", &[]).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2 = json_body(resp2).await;
    assert!(body2.get("status").is_some());

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_td_p4_migrated_summary_and_dto_identities() {
    let fixture = support::ApiFixture::track_d("migrated-test");
    let db_path = fixture._root.path().join("mu.sqlite3");
    let ts = now_ms();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO source_usage_epochs(source, active_epoch, active_parser_version)
             VALUES ('fake-source', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('fake-root', 'fake-source', 'native-root-1', NULL, 'fake-root',
                       'main', 'Fake Root Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads(
                thread_id, source, native_session_id, parent_thread_id, root_session_id,
                agent_role, title, project_name, project_path, project_kind, metadata_model,
                created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
             ) VALUES ('fake-child', 'fake-source', 'native-child-1', 'fake-root', 'fake-root',
                       'subagent', 'Fake Child Title', NULL, NULL, 'unknown', NULL, ?1, ?1, 0, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('fake-source', 1, 'fake-event-1', 'normal', ?1,
                       'fake-root', 'fake-root', NULL, 'fake-model', NULL,
                       10_000_000_000, 500, 100, 0, 500, 0, 1000, 'complete', ?1)",
            [ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events(
                source, source_epoch, event_id, event_kind, occurred_at_ms,
                thread_id, root_session_id, turn_key, model, reasoning_effort,
                estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
             ) VALUES ('fake-source', 1, 'fake-event-2', 'normal', ?1,
                       'fake-child', 'fake-root', NULL, 'fake-model', NULL,
                       5_000_000_000, 200, 50, 0, 200, 0, 400, 'complete', ?1)",
            [ts],
        )
        .unwrap();
    }

    // 1. Public API v1 /v1/usage/summary returns ONLY Codex data
    let response = fixture
        .call(Method::GET, "/api/v1/usage/summary?range=year", &[])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let public_summary = json_body(response).await;
    assert_eq!(public_summary["usage"]["total_tokens"], 0);
    assert_eq!(public_summary["usage"]["session_count"], 0);

    // 2. Internal summary without source filter includes all sources
    let internal_response = fixture
        .call(Method::GET, "/api/usage/summary?range=year", &[])
        .await;
    assert_eq!(internal_response.status(), StatusCode::OK);
    let internal_summary = json_body(internal_response).await;
    assert_eq!(internal_summary["usage"]["total_tokens"], 1400);

    // 3. Filter options: unregistered fake-source is NOT present in terminal options
    let options_response = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    assert_eq!(options_response.status(), StatusCode::OK);
    let options = json_body(options_response).await;
    let sources = options["sources"].as_array().unwrap();
    assert!(!sources.iter().any(|s| s["source"] == "fake-source"));

    // 4. Session list / detail DTOs carry source and native_session_id
    let sessions_response = fixture
        .call(Method::GET, "/api/usage/sessions?range=year", &[])
        .await;
    assert_eq!(sessions_response.status(), StatusCode::OK);
    let sessions = json_body(sessions_response).await;
    let fake_session = sessions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["root_session_id"] == "fake-root")
        .expect("fake-root session in items");
    assert_eq!(fake_session["source"], "fake-source");
    assert_eq!(fake_session["native_session_id"], "native-root-1");

    let fake_index = sessions["sort_index"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["root_session_id"] == "fake-root")
        .expect("fake-root session in sort_index");
    assert_eq!(fake_index["source"], "fake-source");
    assert_eq!(fake_index["native_session_id"], "native-root-1");

    let detail_response = fixture
        .call(
            Method::GET,
            "/api/usage/sessions/fake-root/detail?range=year",
            &[],
        )
        .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail = json_body(detail_response).await;
    assert_eq!(detail["source"], "fake-source");
    assert_eq!(detail["native_session_id"], "native-root-1");
    assert_eq!(detail["main"]["source"], "fake-source");
    assert_eq!(detail["main"]["native_session_id"], "native-root-1");
    let fake_child = detail["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["thread_id"] == "fake-child")
        .expect("fake-child in subagents");
    assert_eq!(fake_child["source"], "fake-source");
    assert_eq!(fake_child["native_session_id"], "native-child-1");

    fixture.scanner.shutdown().unwrap();
}
