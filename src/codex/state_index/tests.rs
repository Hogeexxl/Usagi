use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;
use crate::{
    codex::{
        metadata::{ExistingThread, ResolutionInput, ThreadMetadataResolver},
        rollout::{
            AgentRoleProvenance, Candidate, OwnershipBoundary, OwnershipConfidence,
            ParentHintProvenance, RolloutThreadFact,
        },
        session_index::{SessionNameSnapshot, SessionSourceStatus},
    },
    domain::{AgentRole, MetadataQualityStatus, Patch, ProjectKind},
};

fn fixture_path(name: &str) -> String {
    std::env::temp_dir()
        .join("usagi-state-index")
        .join(name.trim_start_matches('/'))
        .to_string_lossy()
        .into_owned()
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    directory: PathBuf,
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "usagi-state-index-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("state_5.sqlite");
        Self { directory, path }
    }

    fn connection(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn empty_sessions() -> SessionNameSnapshot {
    SessionNameSnapshot {
        names: Default::default(),
        facts: Vec::new(),
        diagnostics: Vec::new(),
        status: SessionSourceStatus::Complete,
    }
}

fn existing(thread_id: &str) -> ExistingThread {
    ExistingThread {
        thread_id: thread_id.to_owned(),
        source: crate::source::SourceId::CODEX,
        parent_thread_id: None,
        root_session_id: None,
        agent_role: AgentRole::Unknown,
        title: None,
        project_name: None,
        project_path: None,
        project_kind: ProjectKind::Unknown,
        metadata_model: None,
        created_at_ms: None,
        updated_at_ms: None,
        archived: false,
        metadata_quality_status: MetadataQualityStatus::Partial,
    }
}

#[test]
fn complete_schema_reads_only_allowlisted_thread_and_edge_facts() {
    let database = TempDb::new();
    let connection = database.connection();
    let rollout_path = fixture_path("sessions/rollout-child.jsonl");
    let cwd_path = fixture_path("work/./project");
    let expected_rollout_path = paths::normalize_source_path(Path::new(&rollout_path))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let expected_cwd_path = paths::normalize_source_path(Path::new(&fixture_path("work/project")))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    connection
        .execute_batch(&format!(
            "CREATE TABLE threads (
                    id TEXT PRIMARY KEY, rollout_path TEXT, created_at INTEGER,
                    created_at_ms INTEGER, updated_at INTEGER, updated_at_ms INTEGER,
                    archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT,
                    agent_role TEXT, first_user_message TEXT, preview TEXT,
                    sandbox_policy TEXT, approval_mode TEXT
                 );
                 CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT, child_thread_id TEXT, status TEXT,
                    observed_at INTEGER, observed_at_ms INTEGER
                 );
                 INSERT INTO threads VALUES (
                    'child', '{rollout_path}', 1, 2000, 3, 4000,
                    1, '{cwd_path}', 'Title', 'Name', 'model-a', 'subagent',
                    'SECRET_BODY', 'SECRET_PREVIEW', 'danger', 'never'
                 );
                 INSERT INTO thread_spawn_edges VALUES ('parent', 'child', 'ready', 5, 6000);"
        ))
        .unwrap();
    drop(connection);

    let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
    assert!(snapshot.is_available());
    let fact = snapshot.thread("child").unwrap();
    assert_eq!(
        fact.rollout_path.as_deref(),
        Some(expected_rollout_path.as_str())
    );
    assert_eq!(fact.created_at_ms, Some(2000));
    assert_eq!(fact.updated_at_ms, Some(4000));
    assert_eq!(fact.archived, Some(true));
    assert_eq!(fact.cwd.as_deref(), Some(expected_cwd_path.as_str()));
    assert_eq!(fact.title.as_deref(), Some("Title"));
    assert_eq!(fact.name.as_deref(), Some("Name"));
    assert_eq!(fact.metadata_model.as_deref(), Some("model-a"));
    assert_eq!(fact.agent_role_hint.as_deref(), Some("subagent"));
    assert_eq!(fact.agent_path, None);
    assert_eq!(snapshot.spawn_edges.len(), 1);
    assert_eq!(snapshot.spawn_edges[0].parent_thread_id, "parent");
    assert_eq!(snapshot.spawn_edges[0].observed_at_ms, Some(6000));
    assert!(!format!("{snapshot:?}").contains("SECRET"));
}

#[test]
fn reads_optional_agent_path_only_when_schema_exposes_it() {
    let database = TempDb::new();
    let connection = database.connection();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    agent_path TEXT,
                    unallowlisted_secret TEXT
                 );
                 INSERT INTO threads VALUES ('child', '  /root/group/./task  ', 'do-not-read');",
        )
        .unwrap();
    drop(connection);

    let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
    let fact = snapshot.thread("child").unwrap();
    assert_eq!(fact.agent_path.as_deref(), Some("/root/group/task"));
    assert!(!format!("{snapshot:?}").contains("do-not-read"));
}

#[test]
fn id_only_and_missing_optional_columns_degrade_without_guessing() {
    let database = TempDb::new();
    let connection = database.connection();
    connection
        .execute_batch("CREATE TABLE threads (id TEXT); INSERT INTO threads VALUES ('only');")
        .unwrap();
    drop(connection);

    let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
    let fact = snapshot.thread("only").unwrap();
    assert!(snapshot.is_available());
    assert_eq!(snapshot.spawn_edges_status, StateSourceStatus::Unavailable);
    assert_eq!(
        fact,
        &StateThreadFact {
            thread_id: "only".to_owned(),
            rollout_path: None,
            created_at_ms: None,
            updated_at_ms: None,
            archived: None,
            title: None,
            name: None,
            cwd: None,
            metadata_model: None,
            agent_role_hint: None,
            agent_path: None,
        }
    );
}

#[test]
fn missing_threads_table_or_id_marks_the_source_unavailable() {
    for schema in [
        "CREATE TABLE other (id TEXT);",
        "CREATE TABLE threads (title TEXT);",
    ] {
        let database = TempDb::new();
        let connection = database.connection();
        connection.execute_batch(schema).unwrap();
        drop(connection);

        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        assert_eq!(snapshot.status, StateSourceStatus::Unavailable);
        assert!(snapshot.threads.is_empty());
        assert!(snapshot.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "missing_required_table" | "missing_required_column"
            )
        }));
    }
}

#[test]
fn absent_spawn_table_allows_rollout_parent_but_never_infers_main() {
    let database = TempDb::new();
    let connection = database.connection();
    connection
        .execute_batch(
            "CREATE TABLE threads (id TEXT); INSERT INTO threads VALUES ('parent');
                 INSERT INTO threads VALUES ('child'); INSERT INTO threads VALUES ('orphan');",
        )
        .unwrap();
    drop(connection);
    let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
    assert_eq!(snapshot.spawn_edges_status, StateSourceStatus::Unavailable);

    let rollout = RolloutThreadFact {
        source_file_id: 1,
        owning_thread_id: "child".to_owned(),
        cwd: None,
        created_at_ms: None,
        latest_context_model: None,
        latest_context_turn_id: None,
        latest_context_at_ms: None,
        latest_context_record_offset: None,
        parent_thread_id_hint: Some(Candidate {
            value: "parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        }),
        agent_role_hint: Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SubagentSource,
            record_offset: 1,
        }),
        agent_path: None,
        ownership_boundary: OwnershipBoundary {
            replay_start_offset: None,
            owning_records_start_offset: Some(0),
            confidence: OwnershipConfidence::Confirmed,
        },
        has_conflict: false,
        relationship_conflict: false,
    };
    let result = ThreadMetadataResolver::resolve(ResolutionInput {
        state_snapshot: snapshot,
        session_name_snapshot: empty_sessions(),
        global_state_snapshot: crate::codex::GlobalStateSnapshot::unavailable(
            crate::codex::GlobalStateStatus::NotPresent,
            Vec::new(),
        ),
        rollout_facts: vec![rollout],
        source_file_observations: Vec::new(),
        existing_threads: vec![existing("parent"), existing("child"), existing("orphan")],
        resolved_at_ms: 10,
    });
    let child = result
        .patches
        .iter()
        .find(|patch| patch.thread_id == "child")
        .unwrap();
    assert_eq!(child.parent_thread_id, Patch::Set("parent".to_owned()));
    assert_eq!(child.agent_role, Patch::Set(AgentRole::Subagent));
    assert!(result.patches.iter().all(|patch| {
        patch.thread_id != "orphan" || patch.agent_role != Patch::Set(AgentRole::Main)
    }));

    let unavailable = ThreadMetadataResolver::resolve(ResolutionInput {
        state_snapshot: StateSnapshot::unavailable(Vec::new()),
        session_name_snapshot: empty_sessions(),
        global_state_snapshot: crate::codex::GlobalStateSnapshot::unavailable(
            crate::codex::GlobalStateStatus::NotPresent,
            Vec::new(),
        ),
        rollout_facts: Vec::new(),
        source_file_observations: Vec::new(),
        existing_threads: vec![existing("unavailable")],
        resolved_at_ms: 11,
    });
    assert!(unavailable.patches.iter().all(|patch| {
        patch.agent_role != Patch::Set(AgentRole::Main)
            && !matches!(patch.root_session_id, Patch::Set(_))
    }));
}

#[test]
fn seconds_and_milliseconds_are_normalized_with_milliseconds_preferred() {
    let database = TempDb::new();
    let connection = database.connection();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                    id TEXT, created_at INTEGER, created_at_ms INTEGER,
                    updated_at INTEGER, updated_at_ms INTEGER
                 );
                 INSERT INTO threads VALUES ('milliseconds', 1, 2345, 3, 4567);
                 INSERT INTO threads VALUES ('seconds', 7, NULL, 8, NULL);
                 INSERT INTO threads VALUES ('fallback', 9, -1, 10, -1);",
        )
        .unwrap();
    drop(connection);

    let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
    assert_eq!(
        snapshot.thread("milliseconds").unwrap().created_at_ms,
        Some(2345)
    );
    assert_eq!(
        snapshot.thread("milliseconds").unwrap().updated_at_ms,
        Some(4567)
    );
    assert_eq!(
        snapshot.thread("seconds").unwrap().created_at_ms,
        Some(7000)
    );
    assert_eq!(
        snapshot.thread("seconds").unwrap().updated_at_ms,
        Some(8000)
    );
    assert_eq!(
        snapshot.thread("fallback").unwrap().created_at_ms,
        Some(9000)
    );
    assert_eq!(
        snapshot.thread("fallback").unwrap().updated_at_ms,
        Some(10_000)
    );
}

#[test]
fn query_columns_are_allowlisted_and_adapter_connection_is_read_only() {
    let available = THREAD_ALLOWLIST
        .iter()
        .copied()
        .chain([
            "first_user_message",
            "preview",
            "sandbox_policy",
            "approval_mode",
        ])
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let selected = selected_columns(&available, THREAD_ALLOWLIST);
    assert_eq!(selected, THREAD_ALLOWLIST);
    for forbidden in [
        "first_user_message",
        "preview",
        "sandbox_policy",
        "approval_mode",
    ] {
        assert!(!selected.contains(&forbidden));
    }

    let database = TempDb::new();
    let connection = database.connection();
    connection
        .execute("CREATE TABLE threads (id TEXT)", [])
        .unwrap();
    drop(connection);
    let read_only = open_read_only_connection(&database.path, DEFAULT_BUSY_TIMEOUT).unwrap();
    assert!(
        read_only
            .execute("INSERT INTO threads VALUES ('write')", [])
            .is_err()
    );
    let query_only: i64 = read_only
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .unwrap();
    assert_eq!(query_only, 1);
}
