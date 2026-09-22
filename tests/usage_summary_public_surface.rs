//! Public surface stability test for UsageSummary, MainSessionDetail, and SessionDetail.
//!
//! Verifies:
//! - [TD-P4-PUBLIC-RUST-01]: `UsageSummary` struct literal construction using public fields.
//! - [TD-P4-DETAIL-RUST-01]: `MainSessionDetail` and `SessionDetail` struct literal construction
//!   using public fields without project fields on the public structs, and signature stability
//!   of `UsageLedger::session_detail_snapshot()`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use usagi::{
    storage::{Ledger, LedgerOptions},
    usage::{
        SummaryQuery, TimeRange, UsageFilter, UsageSummary,
        aggregate::{
            MainModelUsage, MainSessionDetail, SessionDetail, SessionHealthSummary, SubagentDetail,
        },
        ledger::UsageLedger,
    },
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-public-surface-{label}-{}-{stamp}",
            std::process::id()
        ));
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

#[test]
fn t_td_p4_public_rust_01_usage_summary_public_surface() {
    let temp_dir = TempDir::new("usage-summary");
    let ledger = Ledger::open(LedgerOptions::new(temp_dir.path().join("mu.sqlite3")))
        .expect("open temp ledger");
    let usage_ledger = UsageLedger::new(&ledger, &[]);

    let range = TimeRange::new(0, 10_000).expect("valid range");
    let summary = usage_ledger
        .summary(SummaryQuery::new(range, UsageFilter::default()))
        .expect("summary query");
    let valid_totals = summary.totals;

    let constructed = UsageSummary {
        totals: valid_totals,
        session_count: 42,
        cost_incomplete_session_count: 3,
        complete_session_cost_per_million_tokens: Some(1.234),
        health: SessionHealthSummary {
            total_sessions: 45,
            complete_sessions: 39,
            incomplete_sessions: 3,
            error_sessions: 3,
        },
    };

    assert_eq!(constructed.session_count, 42);
    assert_eq!(constructed.cost_incomplete_session_count, 3);
    assert_eq!(
        constructed.complete_session_cost_per_million_tokens,
        Some(1.234)
    );
    assert_eq!(constructed.health.total_sessions, 45);
}

#[test]
fn t_td_p4_detail_rust_01_session_detail_public_surface() {
    let temp_dir = TempDir::new("session-detail");
    let ledger = Ledger::open(LedgerOptions::new(temp_dir.path().join("mu.sqlite3")))
        .expect("open temp ledger");
    let usage_ledger = UsageLedger::new(&ledger, &[]);

    let range = TimeRange::new(0, 10_000).expect("valid range");
    let summary = usage_ledger
        .summary(SummaryQuery::new(range, UsageFilter::default()))
        .expect("summary query");
    let valid_totals = summary.totals;

    let main_detail = MainSessionDetail {
        title: Some("Root Title".into()),
        thread_id: "thread-root".into(),
        source: "codex".into(),
        native_session_id: "native-root".into(),
        root_session_id: "thread-root".into(),
        models_used: vec!["gpt-4o".into()],
        model_usage: vec![MainModelUsage {
            model: "gpt-4o".into(),
            reasoning_effort: None,
            usage: valid_totals.clone(),
        }],
        self_usage: valid_totals.clone(),
        subagent_count: 1,
        inclusive_usage: valid_totals.clone(),
    };

    let session_detail = SessionDetail {
        root_session_id: "thread-root".into(),
        source: "codex".into(),
        native_session_id: "native-root".into(),
        last_activity_at_ms: 12345,
        main: main_detail,
        subagents: vec![SubagentDetail {
            thread_id: "thread-sub".into(),
            source: "codex".into(),
            native_session_id: "native-sub".into(),
            parent_thread_id: Some("thread-root".into()),
            root_session_id: "thread-root".into(),
            title: Some("Subagent Title".into()),
            last_activity_at_ms: 12345,
            model_usage: vec![],
        }],
    };

    assert_eq!(session_detail.root_session_id, "thread-root");
    assert_eq!(session_detail.main.models_used, vec!["gpt-4o"]);
    assert_eq!(session_detail.subagents.len(), 1);

    // Call public session_detail_snapshot to verify signature stability
    let snapshot_res = usage_ledger.session_detail_snapshot(
        range,
        UsageFilter::default(),
        None,
        "thread-root".into(),
    );
    assert!(snapshot_res.is_err());
}
