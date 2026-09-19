use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use usagi::{
    domain::{
        AgentPathProvenance, AgentRole, AgentRoleProvenance, CheckpointProcessingStatus,
        ContinuationState, CwdProvenance, FactQualityStatus, MetadataCheckpointAdvance,
        MetadataCommitBatch, MetadataSourceCommit, MetadataThreadCommit, OwnershipConfidence,
        ParentHintProvenance, Patch, ResolvedThreadPatch, RolloutMetadataFact,
        SafeFactMismatchReason, SafeFactState, SourceArea, SourceObservation,
        SourceObservationBatch, SourceRegionStatus,
    },
    storage::{Ledger, LedgerOptions},
};

type PersistedProvenance = (
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec01-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn db_path(&self) -> PathBuf {
        self.0.join("mu.sqlite3")
    }

    fn codex_home(&self) -> PathBuf {
        self.0.join("codex-fixture")
    }

    fn source_path(&self, file_name: &str) -> String {
        self.0.join("sources").join(file_name).display().to_string()
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(LedgerOptions::new(self.db_path(), self.codex_home()))
            .expect("open temporary ledger")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn observation(
    path: &str,
    device_id: i64,
    inode: i64,
    size: i64,
    mtime: i64,
    seen_at: i64,
) -> SourceObservation {
    SourceObservation::new(
        path,
        SourceArea::Sessions,
        device_id,
        inode,
        size,
        mtime,
        seen_at,
    )
    .expect("valid source observation")
}

fn record_sources(ledger: &Ledger, observations: Vec<SourceObservation>) -> Vec<i64> {
    ledger
        .record_source_observations(
            SourceObservationBatch::new(
                observations,
                SourceRegionStatus::Complete,
                SourceRegionStatus::Complete,
            )
            .expect("valid observation batch"),
        )
        .expect("record source observations")
        .results
        .into_iter()
        .map(|result| result.source_file_id)
        .collect()
}

fn simple_fact(
    source_file_id: i64,
    file_generation: i64,
    parser_version: i64,
    offset: i64,
    owner: &str,
) -> RolloutMetadataFact {
    RolloutMetadataFact {
        source_file_id,
        file_generation,
        metadata_parser_version: parser_version,
        resolved_through_offset: offset,
        owning_thread_id: owner.to_owned(),
        continuation_state: ContinuationState::OwningLive,
        cwd: None,
        cwd_provenance: None,
        cwd_record_offset: None,
        created_at_ms: None,
        latest_context_model: None,
        latest_context_turn_id: None,
        latest_context_at_ms: None,
        parent_thread_id_hint: None,
        parent_hint_provenance: None,
        parent_hint_record_offset: None,
        agent_role_hint: None,
        agent_role_provenance: None,
        agent_role_record_offset: None,
        agent_path: None,
        agent_path_provenance: None,
        agent_path_record_offset: None,
        replay_start_offset: None,
        owning_records_start_offset: None,
        ownership_confidence: OwnershipConfidence::Confirmed,
        fact_quality_status: FactQualityStatus::Complete,
        relationship_conflict: false,
        updated_at_ms: 10,
    }
}

fn source_commit(
    source_file_id: i64,
    file_generation: i64,
    parser_version: i64,
    offset: i64,
    owner: &str,
    fact: RolloutMetadataFact,
) -> MetadataSourceCommit {
    MetadataSourceCommit::new(
        source_file_id,
        file_generation,
        None,
        owner,
        fact,
        MetadataCheckpointAdvance {
            parser_version,
            committed_offset: offset,
            guard_hash: (offset > 0).then(|| vec![0xA5]),
            processing_status: CheckpointProcessingStatus::Ready,
            last_successful_scan_at_ms: Some(10),
            last_error_code: None,
        },
    )
    .expect("valid metadata source commit")
}

fn bootstrap_matching_fact(
    connection: &Connection,
    source_file_id: i64,
    owner: &str,
    parser_version: i64,
    offset: i64,
) {
    connection
        .execute(
            "UPDATE source_files
             SET thread_id = ?2, file_generation = 1, observed_size = ?3, file_status = 'present'
             WHERE source_file_id = ?1",
            params![source_file_id, owner, offset],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE source_checkpoints
             SET parser_version = ?2, committed_offset = ?3,
                 guard_hash = X'01', processing_status = 'ready'
             WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
            params![source_file_id, parser_version, offset],
        )
        .unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO rollout_metadata_facts (
                source_file_id, file_generation, metadata_parser_version,
                resolved_through_offset, owning_thread_id, continuation_state,
                ownership_confidence, fact_quality_status, updated_at_ms
             ) VALUES (?1, 1, ?2, ?3, ?4, 'owning_live', 'confirmed', 'complete', 10)",
            params![source_file_id, parser_version, offset, owner],
        )
        .unwrap();
}

#[test]
fn t_s01_001_v1_schema_initial_state_pragmas_and_reopen_matrix() {
    // Version-1 is intentionally isolated from Ledger::open(), because the
    // current binary migrates a new database all the way to the latest schema.
    let v1_root = TempRoot::new("v1-schema");
    let v1_db = v1_root.path().join("v1.sqlite3");
    let v1 = Connection::open(&v1_db).unwrap();
    v1.execute_batch(include_str!("../src/storage/schema/0001_initial.sql"))
        .unwrap();
    v1.pragma_update(None, "user_version", 1).unwrap();

    let tables = v1
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        tables,
        vec![
            "app_meta",
            "rollout_metadata_facts",
            "scan_runs",
            "source_checkpoints",
            "source_files",
            "threads",
        ]
    );
    assert_eq!(
        v1.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        v1.query_row("SELECT count(*) FROM scan_runs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let initial: (
        i64,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = v1
        .query_row(
            "SELECT metadata_parser_version, data_revision, status_revision, scan_state,
                    active_scan_id, followup_scan_id, followup_state
             FROM app_meta WHERE id = 1",
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
    assert_eq!(initial, (0, 0, 0, "idle".into(), None, None, None));
    drop(v1);

    let latest_root = TempRoot::new("latest-reopen");
    let ledger = latest_root.ledger();
    let reopen_path = latest_root.source_path("reopen.jsonl");
    let pragmas = ledger.pragma_state().unwrap();
    assert!(pragmas.journal_mode_wal);
    assert!(pragmas.synchronous_normal);
    assert!(pragmas.foreign_keys);
    assert_eq!(pragmas.busy_timeout_ms, 5_000);

    let app = ledger.app_state().unwrap();
    assert_eq!(app.data_revision, 1);
    assert_eq!(app.scan.status_revision, 0);
    assert!(app.scan.active_scan_id.is_none());
    assert!(app.scan.followup_scan_id.is_none());
    assert!(app.scan.followup_state.is_none());

    let source_file_id =
        record_sources(&ledger, vec![observation(&reopen_path, 101, 201, 32, 1, 1)])[0];
    let schema_version = ledger.schema_version().unwrap();
    drop(ledger);

    let reopened = latest_root.ledger();
    assert_eq!(reopened.schema_version().unwrap(), schema_version);
    let state = reopened.load_metadata_scan_state([source_file_id]).unwrap();
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.entries[0].source.current_path, reopen_path);
    let connection = Connection::open(reopened.database_path()).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM scan_runs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn t_s01_002_source_identity_database_constraints_matrix() {
    let root = TempRoot::new("source-constraints");
    let ledger = root.ledger();
    let connection = Connection::open(ledger.database_path()).unwrap();

    let insert = |id: i64, path: &str, device: i64, inode: i64, generation: i64, size: i64| {
        connection.execute(
            "INSERT INTO source_files (
                source_file_id, thread_id, current_path, source_area,
                device_id, inode, file_generation, observed_size,
                observed_mtime_ns, file_status, last_seen_at_ms
             ) VALUES (?1, NULL, ?2, 'sessions', ?3, ?4, ?5, ?6, 1, 'present', 1)",
            params![id, path, device, inode, generation, size],
        )
    };

    insert(1, "/tmp/usagi/spec01/a.jsonl", 10, 20, 1, 10).unwrap();
    assert!(insert(2, "/tmp/usagi/spec01/a.jsonl", 11, 21, 1, 10).is_err());
    assert!(insert(3, "/tmp/usagi/spec01/b.jsonl", 10, 20, 1, 10).is_err());
    assert!(insert(4, "/tmp/usagi/spec01/c.jsonl", 12, 22, 0, 10).is_err());
    assert!(insert(5, "/tmp/usagi/spec01/d.jsonl", 13, 23, 1, -1).is_err());

    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM source_files", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn t_s01_005_safe_fact_reuse_mismatch_matrix() {
    let root = TempRoot::new("safe-fact-matrix");
    let ledger = root.ledger();
    let safe_fact_path = root.source_path("safe-fact.jsonl");
    let source_file_id = record_sources(
        &ledger,
        vec![observation(&safe_fact_path, 301, 401, 64, 1, 1)],
    )[0];
    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    drop(connection);

    let assert_state = |expected: SafeFactState| {
        let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
        assert_eq!(state.entries[0].safe_fact, expected);
    };
    assert!(matches!(
        ledger
            .load_metadata_scan_state([source_file_id])
            .unwrap()
            .entries[0]
            .safe_fact,
        SafeFactState::Matching(_)
    ));

    let connection = Connection::open(ledger.database_path()).unwrap();
    connection
        .execute(
            "UPDATE rollout_metadata_facts SET file_generation = 2 WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(
        SafeFactMismatchReason::GenerationMismatch,
    ));

    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    connection
        .execute(
            "UPDATE rollout_metadata_facts SET metadata_parser_version = 6 WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(
        SafeFactMismatchReason::ParserVersionMismatch,
    ));

    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    connection
        .execute(
            "UPDATE rollout_metadata_facts SET resolved_through_offset = 63 WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(SafeFactMismatchReason::OffsetMismatch));

    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    connection
        .execute(
            "UPDATE source_files SET thread_id = NULL WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(
        SafeFactMismatchReason::BindingMismatch,
    ));

    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    connection
        .execute(
            "UPDATE source_files SET thread_id = 'thread-2' WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(
        SafeFactMismatchReason::OwningThreadMismatch,
    ));

    let connection = Connection::open(ledger.database_path()).unwrap();
    bootstrap_matching_fact(&connection, source_file_id, "thread-1", 7, 64);
    connection
        .execute(
            "UPDATE source_files SET file_status = 'missing' WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    drop(connection);
    assert_state(SafeFactState::Stale(SafeFactMismatchReason::SourceMissing));
}

#[test]
fn t_mu03_a01_agent_path_safe_fact_provenance_round_trip_and_offset_bound() {
    let root = TempRoot::new("fact-offsets");
    let ledger = root.ledger();
    let offsets_path = root.source_path("offsets.jsonl");
    let source_file_id = record_sources(
        &ledger,
        vec![observation(&offsets_path, 501, 601, 80, 1, 1)],
    )[0];

    let mut fact = simple_fact(source_file_id, 1, 3, 80, "thread-offsets");
    fact.cwd = Some("/tmp/project".into());
    fact.cwd_provenance = Some(CwdProvenance::TurnContext);
    fact.cwd_record_offset = Some(11);
    fact.parent_thread_id_hint = Some("parent-thread".into());
    fact.parent_hint_provenance = Some(ParentHintProvenance::ForkedFromId);
    fact.parent_hint_record_offset = Some(12);
    fact.agent_role_hint = Some("subagent".into());
    fact.agent_role_provenance = Some(AgentRoleProvenance::SubagentSource);
    fact.agent_role_record_offset = Some(13);
    fact.agent_path = Some("/root/gate_b_rereview".into());
    fact.agent_path_provenance = Some(AgentPathProvenance::SessionMeta);
    fact.agent_path_record_offset = Some(14);

    let commit = source_commit(source_file_id, 1, 3, 80, "thread-offsets", fact);
    ledger
        .commit_metadata(
            MetadataCommitBatch::new(vec![
                MetadataThreadCommit::new("thread-offsets", None, vec![commit]).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();

    let connection = Connection::open(ledger.database_path()).unwrap();
    let persisted: PersistedProvenance = connection
        .query_row(
            "SELECT cwd_provenance, cwd_record_offset,
                    parent_hint_provenance, parent_hint_record_offset,
                    agent_role_provenance, agent_role_record_offset
             FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_file_id],
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
        persisted,
        (
            Some("turn_context".into()),
            Some(11),
            Some("forked_from_id".into()),
            Some(12),
            Some("subagent_source".into()),
            Some(13),
        )
    );

    let state = ledger.load_metadata_scan_state([source_file_id]).unwrap();
    let SafeFactState::Matching(restored) = state.entries[0].safe_fact.clone() else {
        panic!("agent_path safe fact did not survive metadata -> source round trip");
    };
    assert_eq!(
        restored.agent_path.as_deref(),
        Some("/root/gate_b_rereview")
    );
    assert_eq!(
        restored.agent_path_provenance,
        Some(AgentPathProvenance::SessionMeta)
    );
    assert_eq!(restored.agent_path_record_offset, Some(14));
    assert!(restored.agent_path_record_offset.unwrap() <= restored.resolved_through_offset);
}

#[test]
fn t_s01_008_batch_metadata_state_is_one_sqlite_snapshot() {
    let root = TempRoot::new("snapshot");
    let ledger = Arc::new(root.ledger());
    let snapshot_a_path = root.source_path("snapshot-a.jsonl");
    let snapshot_b_path = root.source_path("snapshot-b.jsonl");
    let ids = record_sources(
        &ledger,
        vec![
            observation(&snapshot_a_path, 701, 801, 64, 1, 1),
            observation(&snapshot_b_path, 702, 802, 64, 1, 1),
        ],
    );
    let connection = Connection::open(ledger.database_path()).unwrap();
    for (index, source_file_id) in ids.iter().copied().enumerate() {
        bootstrap_matching_fact(
            &connection,
            source_file_id,
            &format!("snapshot-thread-{index}"),
            9,
            64,
        );
    }
    drop(connection);

    let barrier = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let db_path = ledger.database_path().to_path_buf();
    let writer_barrier = Arc::clone(&barrier);
    let writer_done = Arc::clone(&done);
    let writer_ids = ids.clone();
    let writer = thread::spawn(move || {
        let mut connection = Connection::open(db_path).unwrap();
        connection.busy_timeout(Duration::from_secs(5)).unwrap();
        writer_barrier.wait();
        for epoch in 0..250 {
            let offset = if epoch % 2 == 0 { 65 } else { 64 };
            let tx = connection.transaction().unwrap();
            for source_file_id in &writer_ids {
                tx.execute(
                    "UPDATE source_files SET observed_size = ?2 WHERE source_file_id = ?1",
                    params![source_file_id, offset],
                )
                .unwrap();
                tx.execute(
                    "UPDATE source_checkpoints
                     SET committed_offset = ?2, guard_hash = X'01'
                     WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                    params![source_file_id, offset],
                )
                .unwrap();
                tx.execute(
                    "UPDATE rollout_metadata_facts
                     SET resolved_through_offset = ?2
                     WHERE source_file_id = ?1",
                    params![source_file_id, offset],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        writer_done.store(true, Ordering::Release);
    });

    barrier.wait();
    let mut reads = 0usize;
    while reads < 300 || !done.load(Ordering::Acquire) {
        let state = ledger.load_metadata_scan_state(&ids).unwrap();
        assert_eq!(state.entries.len(), 2);
        let epoch = state.entries[0].source.observed_size;
        for entry in &state.entries {
            assert_eq!(entry.source.observed_size, epoch);
            let checkpoint = entry
                .metadata_checkpoint
                .as_ref()
                .expect("metadata checkpoint");
            assert_eq!(checkpoint.committed_offset, epoch);
            match &entry.safe_fact {
                SafeFactState::Matching(fact) => {
                    assert_eq!(fact.resolved_through_offset, epoch);
                }
                other => panic!("expected matching safe fact, got {other:?}"),
            }
        }
        reads += 1;
    }
    writer.join().unwrap();
}

#[test]
fn t_s01_009_metadata_transaction_rollback_survives_reopen() {
    // First prove that a real commit_metadata failure after earlier writes in
    // the transaction leaves no partial state after the database is reopened.
    let root = TempRoot::new("metadata-rollback-reopen");
    let ledger = root.ledger();
    let rollback_path = root.source_path("rollback.jsonl");
    let source_file_id = record_sources(
        &ledger,
        vec![observation(&rollback_path, 901, 1001, 80, 1, 1)],
    )[0];

    let connection = Connection::open(ledger.database_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER spec01_abort_metadata_checkpoint
             BEFORE UPDATE ON source_checkpoints
             WHEN NEW.consumer_kind = 'metadata'
             BEGIN
                 SELECT RAISE(ABORT, 'spec01 injected metadata failure');
             END;",
        )
        .unwrap();
    drop(connection);

    let mut patch = ResolvedThreadPatch::new("thread-rollback", 10).unwrap();
    patch.agent_role = Patch::Set(AgentRole::Main);
    patch.title = Patch::Set("rollback-title".into());
    let fact = simple_fact(source_file_id, 1, 1, 80, "thread-rollback");
    let commit = source_commit(source_file_id, 1, 1, 80, "thread-rollback", fact);
    let result = ledger.commit_metadata(
        MetadataCommitBatch::new(vec![
            MetadataThreadCommit::new("thread-rollback", Some(patch), vec![commit]).unwrap(),
        ])
        .unwrap(),
    );
    assert!(result.is_err());
    drop(ledger);

    let reopened = root.ledger();
    let connection = Connection::open(reopened.database_path()).unwrap();
    let thread_id: Option<String> = connection
        .query_row(
            "SELECT thread_id FROM source_files WHERE source_file_id = ?1",
            [source_file_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(thread_id.is_none());
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let checkpoint: (i64, String) = connection
        .query_row(
            "SELECT committed_offset, processing_status
             FROM source_checkpoints
             WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
            [source_file_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(checkpoint, (0, "pending".into()));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM threads WHERE thread_id = 'thread-rollback'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
        1
    );
    drop(connection);
    drop(reopened);

    // Then simulate the crash window literally: begin a transaction, make
    // several mutually dependent writes, drop the connection without COMMIT,
    // reopen, and verify that none survived.
    let crash_root = TempRoot::new("metadata-uncommitted-drop");
    let crash_ledger = crash_root.ledger();
    let uncommitted_path = crash_root.source_path("uncommitted.jsonl");
    let crash_source = record_sources(
        &crash_ledger,
        vec![observation(&uncommitted_path, 902, 1002, 80, 1, 1)],
    )[0];
    let crash_db = crash_ledger.database_path().to_path_buf();
    let crash_home = crash_root.codex_home();
    drop(crash_ledger);

    let connection = Connection::open(&crash_db).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute(
            "UPDATE source_files SET thread_id = 'thread-crash' WHERE source_file_id = ?1",
            [crash_source],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rollout_metadata_facts (
                source_file_id, file_generation, metadata_parser_version,
                resolved_through_offset, owning_thread_id, continuation_state,
                ownership_confidence, fact_quality_status, updated_at_ms
             ) VALUES (?1, 1, 1, 80, 'thread-crash', 'owning_live', 'confirmed', 'complete', 10)",
            [crash_source],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE source_checkpoints SET parser_version = 1, committed_offset = 80,
                    guard_hash = X'01', processing_status = 'ready'
             WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
            [crash_source],
        )
        .unwrap();
    drop(connection); // no COMMIT: SQLite must roll the transaction back.

    let reopened = Ledger::open(LedgerOptions::new(&crash_db, crash_home)).unwrap();
    let connection = Connection::open(reopened.database_path()).unwrap();
    let thread_id: Option<String> = connection
        .query_row(
            "SELECT thread_id FROM source_files WHERE source_file_id = ?1",
            [crash_source],
            |row| row.get(0),
        )
        .unwrap();
    assert!(thread_id.is_none());
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [crash_source],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT committed_offset FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [crash_source],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn t_s01_011_generation_change_deletes_persisted_safe_fact() {
    let root = TempRoot::new("generation-fact-delete");
    let ledger = root.ledger();
    let generation_path = root.source_path("generation.jsonl");
    let source_file_id = record_sources(
        &ledger,
        vec![observation(&generation_path, 1101, 1201, 100, 100, 1)],
    )[0];

    let fact = simple_fact(source_file_id, 1, 1, 80, "thread-generation");
    let commit = source_commit(source_file_id, 1, 1, 80, "thread-generation", fact);
    ledger
        .commit_metadata(
            MetadataCommitBatch::new(vec![
                MetadataThreadCommit::new("thread-generation", None, vec![commit]).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();

    let connection = Connection::open(ledger.database_path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    drop(connection);

    let outcome = ledger
        .record_source_observations(
            SourceObservationBatch::new(
                vec![observation(&generation_path, 1101, 1201, 100, 101, 2)],
                SourceRegionStatus::Complete,
                SourceRegionStatus::Complete,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(outcome.results[0].source_file_id, source_file_id);
    assert_eq!(outcome.results[0].file_generation, 2);
    assert!(outcome.results[0].replaced);

    let connection = Connection::open(ledger.database_path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn t_s01_013_deleting_source_cascades_all_consumer_checkpoints() {
    let root = TempRoot::new("checkpoint-cascade");
    let ledger = root.ledger();
    let cascade_path = root.source_path("cascade.jsonl");
    let source_file_id = record_sources(
        &ledger,
        vec![observation(&cascade_path, 1301, 1401, 100, 1, 1)],
    )[0];

    let connection = Connection::open(ledger.database_path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_checkpoints (
                source_file_id, consumer_kind, parser_version,
                committed_offset, guard_hash, processing_status
             ) VALUES (?1, 'usage', 1, 0, NULL, 'pending')",
            [source_file_id],
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM source_checkpoints WHERE source_file_id = ?1",
                [source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );

    connection
        .execute(
            "DELETE FROM source_files WHERE source_file_id = ?1",
            [source_file_id],
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM source_checkpoints WHERE source_file_id = ?1",
                [source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary() {
    let root = TempRoot::new("privacy-schema");
    let ledger = root.ledger();
    let connection = Connection::open(ledger.database_path()).unwrap();

    let table_names = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let forbidden_columns = [
        "body",
        "content",
        "message",
        "messages",
        "payload",
        "raw_json",
        "raw_line",
        "preview",
        "first_user_message",
        "prompt",
        "response",
        "text",
    ];
    for table in table_names {
        let quoted = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info(\"{quoted}\")"))
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for forbidden in forbidden_columns {
            assert!(
                !columns.iter().any(|column| column == forbidden),
                "table {table} contains forbidden body column {forbidden}"
            );
        }
    }
    drop(connection);

    // Guard the entire checked-in Rust test corpus against accidentally using
    // Ledger's real-home fallback. Tests must pass an explicit temporary
    // CODEX_HOME fixture instead.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let forbidden_test_patterns = [
        ["LedgerOptions::", "for_database("].concat(),
        ["LedgerOptions::", "default()"].concat(),
        ["join(\"", ".codex", "\")"].concat(),
        ["\"~/", ".codex", "\""].concat(),
    ];

    let mut rust_files = Vec::new();
    collect_rust_files(&manifest.join("src"), &mut rust_files);
    collect_rust_files(&manifest.join("tests"), &mut rust_files);
    for path in rust_files {
        let source = fs::read_to_string(&path).unwrap();
        let test_source = if path.starts_with(manifest.join("src")) {
            source
                .find("#[cfg(test)]")
                .map(|offset| &source[offset..])
                .unwrap_or("")
        } else {
            source.as_str()
        };
        for pattern in &forbidden_test_patterns {
            assert!(
                !test_source.contains(pattern),
                "test source {} contains forbidden real-home access pattern",
                path.display()
            );
        }
    }

    // There is no content/error logging sink in the storage/scanner/codex/usage
    // modules today. Keep it that way unless a logger is introduced together
    // with a sentinel-redaction test.
    for module in ["storage", "scanner", "codex", "usage"] {
        let mut module_files = Vec::new();
        collect_rust_files(&manifest.join("src").join(module), &mut module_files);
        for path in module_files {
            let source = fs::read_to_string(&path).unwrap();
            for macro_name in ["println!(", "eprintln!(", "dbg!(", "tracing::", "log::"] {
                assert!(
                    !source.contains(macro_name),
                    "{} introduces an unchecked logging sink",
                    path.display()
                );
            }
        }
    }
}

fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
