#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use super::*;
    use crate::codex::domain::CheckpointProcessingStatus;
    use crate::codex::normalization::usage_fingerprint;
    use crate::codex::storage::usage::write_turn;
    use crate::codex::storage::usage::*;
    use crate::codex::storage::{CodexStorage, CodexStorageError};
    use crate::source::{SourceId, SourceStorage};
    use crate::storage::Ledger;
    use crate::storage::LedgerOptions;
    use crate::usage::event::EventKind;
    use crate::usage::normalized::NormalizedTokenUsage;
    use rusqlite::params;

    mod spec04_p2;
    mod usage_incremental_scan;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        ledger: Ledger,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "usagi-storage-usage-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("codex")).unwrap();
            let ledger = Ledger::open(LedgerOptions::new(root.join("mu.sqlite3"))).unwrap();
            {
                let connection = ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_usage_epochs
                         SET active_epoch=1,active_parser_version=?1
                         WHERE source='codex'",
                        [crate::codex::normalization::USAGE_PARSER_VERSION],
                    )
                    .unwrap();
                for (thread_id, parent, root_id, role) in [
                    ("root", None, Some("root"), "main"),
                    ("child", Some("root"), Some("root"), "subagent"),
                    ("unresolved", None, None, "unknown"),
                    ("other-root", None, Some("other-root"), "main"),
                ] {
                    connection
                        .execute(
                            "INSERT INTO threads (
                                thread_id,source,native_session_id,parent_thread_id,root_session_id,
                                agent_role,project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                             ) VALUES (?1,'codex',?1,?2,?3,?4,'unknown',0,'complete',1)",
                            params![thread_id, parent, root_id, role],
                        )
                        .unwrap();
                }
            }
            Self { root, ledger }
        }

        fn add_source(&self, id: i64, thread_id: Option<&str>, device: i64) {
            let connection = self.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files (
                        source_file_id,thread_id,current_path,source_area,device_id,inode,
                        file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                     ) VALUES (?1,?2,?3,'sessions',?4,?5,1,100,1,'present',1)",
                    params![
                        id,
                        thread_id,
                        format!("/tmp/usage-{id}.jsonl"),
                        device,
                        device
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_checkpoints (
                     source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                     processing_status,last_successful_scan_at_ms,last_error_code
                     ) VALUES (?1,'metadata',1,80,?2,'ready',1,NULL),
                              (?1,'usage',?3,0,NULL,'pending',NULL,NULL)",
                    params![
                        id,
                        vec![8_u8; 32],
                        crate::codex::normalization::USAGE_PARSER_VERSION
                    ],
                )
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn with_codex<T>(
        ledger: &Ledger,
        operation: impl FnOnce(&CodexStorage<'_>) -> T,
    ) -> T {
        let reopened = Arc::new(
            Ledger::open(LedgerOptions::new(ledger.database_path()))
                .expect("open Codex test storage"),
        );
        {
            let connection = reopened.connection().expect("open Codex test connection");
            let binding_status: String = connection
                .query_row(
                    "SELECT binding_status FROM codex_adapter_state WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .expect("read Codex test binding state");
            if binding_status == "unbound" {
                connection
                    .execute(
                        "UPDATE codex_adapter_state
                         SET home_fingerprint='test-fixture',binding_status='ready'
                         WHERE id=1",
                        [],
                    )
                    .expect("bind Codex test storage");
            }
        }
        let source = SourceStorage::with_ledger("test", SourceId::CODEX, reopened);
        let storage = CodexStorage::new(&source).expect("create Codex test storage");
        operation(&storage)
    }

    fn insert_gc_thread(connection: &rusqlite::Connection, source: &str, thread_id: &str) {
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

    fn insert_gc_event(
        connection: &rusqlite::Connection,
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
        let fixture = Fixture::new();
        let connection = fixture.ledger.connection().unwrap();
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
        insert_gc_thread(&connection, "codex", "gc-codex-root");
        insert_gc_thread(&connection, "fake-source", "gc-fake-root");
        insert_gc_event(
            &connection,
            "codex",
            2,
            "codex-active",
            "gc-codex-root",
            200,
        );
        insert_gc_event(&connection, "codex", 3, "codex-build", "gc-codex-root", 300);
        insert_gc_event(
            &connection,
            "codex",
            1,
            "codex-inactive",
            "gc-codex-root",
            100,
        );
        insert_gc_event(
            &connection,
            "fake-source",
            8,
            "fake-active",
            "gc-fake-root",
            800,
        );
        insert_gc_event(
            &connection,
            "fake-source",
            7,
            "fake-inactive",
            "gc-fake-root",
            700,
        );
        drop(connection);

        let mut deleted = 0;
        for _ in 0..8 {
            let page = with_codex(&fixture.ledger, |storage| storage.cleanup_inactive(1)).unwrap();
            deleted += page;
            if page == 0 {
                break;
            }
        }
        assert_eq!(deleted, 1);

        let connection = fixture.ledger.connection().unwrap();
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
                ("fake-source".to_owned(), 7, "fake-inactive".to_owned()),
                ("fake-source".to_owned(), 8, "fake-active".to_owned()),
            ]
        );
    }

    fn vector() -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(10, 2, Some(3), 4, 1, 14).unwrap()
    }

    fn state(
        thread_id: &str,
        root_id: &str,
        device: i64,
        offset: i64,
        active_turn: bool,
    ) -> UsageSourceStateWrite {
        let value = vector();
        UsageSourceStateWrite {
            file_generation: 1,
            device_id: device,
            inode: device,
            usage_parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            canonical_algorithm_version:
                crate::codex::normalization::USAGE_CANONICAL_ALGORITHM_VERSION,
            resolved_through_offset: offset,
            observed_raw_size: 100,
            raw_tail_status: UsageTailStatus::Unverified,
            raw_tail_start_offset: None,
            owning_thread_id: thread_id.to_owned(),
            root_session_id: root_id.to_owned(),
            continuation_state: UsageContinuationState::OwningLive,
            previous_total: Some(UsageSnapshot {
                fingerprint: usage_fingerprint(&value).to_vec(),
                vector: value,
            }),
            previous_total_offset: Some(offset),
            chain_state: UsageChainState::Continuous,
            active_turn_key: active_turn.then(|| "turn".to_owned()),
            active_model: Some("model".to_owned()),
            active_model_offset: Some(0),
            active_reasoning_effort: None,
            active_reasoning_effort_offset: None,
            updated_at_ms: 10,
        }
    }

    fn source_commit(
        source_id: i64,
        device: i64,
        thread_id: &str,
        root_id: &str,
        event_id: char,
        with_auxiliary_rows: bool,
    ) -> UsageSourceCommit {
        let event_id = event_id.to_string().repeat(64);
        let value = vector();
        let snapshot = UsageSnapshot {
            fingerprint: usage_fingerprint(&value).to_vec(),
            vector: value.clone(),
        };
        UsageSourceCommit {
            source_file_id: source_id,
            expected_file_generation: 1,
            expected_previous_thread_id: Some(thread_id.to_owned()),
            expected_checkpoint: UsageCheckpointExpectation {
                parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
                committed_offset: 0,
                guard_hash: None,
                processing_status: CheckpointProcessingStatus::Pending,
            },
            expected_checkpoint_missing: false,
            expected_state: None,
            local_replay: false,
            batch_start_offset: 0,
            fixed_observed_raw_size: 100,
            last_complete_offset: 20,
            source_bytes_consumed: 20,
            complete_line_count: 1,
            candidate_count: 1,
            replayed_prefix_bytes: 0,
            replayed_prefix_lines: 0,
            fixed_view_exhausted: false,
            tail_status: UsageTailStatus::Unverified,
            tail_start_offset: None,
            events: vec![UsageEventWrite {
                event_id: event_id.clone(),
                kind: EventKind::Normal,
                occurred_at_ms: 5,
                thread_id: thread_id.to_owned(),
                root_session_id: root_id.to_owned(),
                turn_key: None,
                model: "model".to_owned(),
                reasoning_effort: None,
                estimated_cost_nanos_usd: None,
                usage: value.clone(),
                created_at_ms: 10,
            }],
            occurrences: vec![UsageOccurrenceWrite {
                source_file_id: source_id,
                file_generation: 1,
                source_start_offset: 0,
                source_end_offset: 20,
                event_id,
            }],
            skill_events: Vec::new(),
            turns: with_auxiliary_rows
                .then(|| UsageTurnWrite {
                    turn_key: "turn".to_owned(),
                    raw_turn_id: None,
                    started_at_ms: Some(1),
                    ended_at_ms: None,
                    start_offset: 0,
                    end_offset: None,
                    status: UsageTurnStatus::Open,
                    start_total: None,
                    last_total: Some(snapshot.clone()),
                    accounted: snapshot.clone(),
                    accounted_candidate_count: 1,
                    model_state: UsageTurnModelState::Single("model".to_owned()),
                    reasoning_effort_state: UsageTurnReasoningEffortState::None,
                    unresolved_reasoning_effort_seen: false,
                    unresolved_model_seen: false,
                    blocks: UsageCompensationBlocks {
                        start_missing: true,
                        ..UsageCompensationBlocks::default()
                    },
                    quality_status: "partial",
                    state_through_offset: 20,
                    updated_at_ms: 10,
                })
                .into_iter()
                .collect(),
            anomalies: with_auxiliary_rows
                .then(|| UsageAnomalyWrite {
                    anomaly_id: "b".repeat(64),
                    detected_at_ms: 10,
                    occurred_at_ms: Some(5),
                    kind: UsageAnomalyKind::TurnReplaced,
                    severity_error: false,
                    source_start_offset: Some(0),
                })
                .into_iter()
                .collect(),
            updated_state: state(thread_id, root_id, device, 20, with_auxiliary_rows),
            next_guard_hash: Some(vec![9; 32]),
            committed_at_ms: 10,
        }
    }

    fn batch(thread_id: &str, root_id: &str, source: UsageSourceCommit) -> UsageCommitBatch {
        UsageCommitBatch {
            ledger_epoch: 1,
            usage_parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            thread_id: thread_id.to_owned(),
            root_session_id: root_id.to_owned(),
            sources: vec![source],
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the test helper mirrors one continuation commit's proof fields"
    )]
    fn continuation_source(
        source: &mut UsageSourceCommit,
        expected_state: UsageSourceStateWrite,
        expected_offset: i64,
        expected_guard_hash: Vec<u8>,
        next_offset: i64,
        next_guard_hash: Vec<u8>,
        accounted_candidate_count: i64,
        usage: NormalizedTokenUsage,
        reasoning_effort_state: UsageTurnReasoningEffortState,
        unresolved_reasoning_effort_seen: bool,
    ) {
        source.expected_checkpoint = UsageCheckpointExpectation {
            parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            committed_offset: expected_offset,
            guard_hash: Some(expected_guard_hash),
            processing_status: CheckpointProcessingStatus::Ready,
        };
        source.expected_state = Some(expected_state);
        source.batch_start_offset = expected_offset;
        source.last_complete_offset = next_offset;
        source.source_bytes_consumed = next_offset - expected_offset;
        source.next_guard_hash = Some(next_guard_hash);
        source.committed_at_ms = next_offset;

        source.events[0].occurred_at_ms = next_offset;
        source.events[0].turn_key = Some("turn".to_owned());
        source.events[0].reasoning_effort = match &reasoning_effort_state {
            UsageTurnReasoningEffortState::Single(value) => Some(value.clone()),
            UsageTurnReasoningEffortState::None | UsageTurnReasoningEffortState::Mixed => None,
        };
        source.events[0].usage = usage.clone();
        source.occurrences[0].source_start_offset = expected_offset;
        source.occurrences[0].source_end_offset = next_offset;

        let snapshot = UsageSnapshot {
            fingerprint: usage_fingerprint(&usage).to_vec(),
            vector: usage,
        };
        let turn = &mut source.turns[0];
        turn.last_total = Some(snapshot.clone());
        turn.accounted = snapshot;
        turn.accounted_candidate_count = accounted_candidate_count;
        turn.reasoning_effort_state = reasoning_effort_state.clone();
        turn.unresolved_reasoning_effort_seen = unresolved_reasoning_effort_seen;
        turn.state_through_offset = next_offset;
        turn.updated_at_ms = next_offset;

        let thread_id = source.updated_state.owning_thread_id.clone();
        let root_id = source.updated_state.root_session_id.clone();
        let device = source.updated_state.device_id;
        source.updated_state = state(&thread_id, &root_id, device, next_offset, true);
        source.updated_state.active_reasoning_effort = match &reasoning_effort_state {
            UsageTurnReasoningEffortState::Single(value) => Some(value.clone()),
            UsageTurnReasoningEffortState::None | UsageTurnReasoningEffortState::Mixed => None,
        };
        let effort_offset = source
            .updated_state
            .active_reasoning_effort
            .as_ref()
            .map(|_| next_offset);
        source.updated_state.active_reasoning_effort_offset = effort_offset;
        source.updated_state.updated_at_ms = next_offset;
    }

    fn durable_turn_snapshot(
        transaction: &Connection,
        source_file_id: i64,
    ) -> (String, Option<String>, i64, i64, i64, i64) {
        transaction
            .query_row(
                "SELECT reasoning_effort_state,single_reasoning_effort,
                        accounted_total_tokens,accounted_candidate_count,
                        state_through_offset,updated_at_ms
                 FROM codex_turns
                 WHERE ledger_epoch=1 AND source_file_id=?1
                   AND file_generation=1 AND turn_key='turn'",
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
            .unwrap()
    }

    #[test]
    fn atomic_commit_duplicate_occurrence_and_conflict_matrix() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        let first = batch(
            "child",
            "root",
            source_commit(1, 11, "child", "root", 'a', true),
        );
        let outcome = with_codex(&fixture.ledger, |storage| {
            storage.commit_group(first.clone())
        })
        .unwrap();
        assert_eq!(
            (
                outcome.events_inserted,
                outcome.events_deduplicated,
                outcome.data_revision
            ),
            (1, 0, 1)
        );
        let connection = fixture.ledger.connection().unwrap();
        let facts: (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM codex_usage_event_occurrences),
                    (SELECT count(*) FROM codex_turns),
                    (SELECT count(*) FROM codex_ingest_anomalies),
                    (SELECT count(*) FROM codex_usage_source_states),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='metadata')",
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
        assert_eq!(facts, (1, 1, 1, 1, 1, 80));
        drop(connection);

        fixture.add_source(2, Some("child"), 12);
        let duplicate = batch(
            "child",
            "root",
            source_commit(2, 12, "child", "root", 'a', false),
        );
        let outcome = with_codex(&fixture.ledger, |storage| {
            storage.commit_group(duplicate.clone())
        })
        .unwrap();
        assert_eq!(
            (
                outcome.events_inserted,
                outcome.events_deduplicated,
                outcome.data_revision
            ),
            (0, 1, 1)
        );
        let connection = fixture.ledger.connection().unwrap();
        let counts: (i64, i64) = connection
            .query_row("SELECT (SELECT count(*) FROM usage_events),(SELECT count(*) FROM codex_usage_event_occurrences)", [], |row| Ok((row.get(0)?,row.get(1)?)))
            .unwrap();
        assert_eq!(counts, (1, 2));
        drop(connection);

        fixture.add_source(3, Some("child"), 13);
        let mut conflict = source_commit(3, 13, "child", "root", 'a', true);
        conflict.events[0].model = "conflicting-model".to_owned();
        assert!(
            with_codex(&fixture.ledger, |storage| storage
                .commit_group(batch("child", "root", conflict)))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let rolled_back: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM codex_usage_event_occurrences WHERE source_file_id=3),
                    (SELECT count(*) FROM codex_usage_source_states WHERE source_file_id=3),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=3 AND consumer_kind='usage')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0));
        drop(connection);

        fixture.add_source(4, Some("child"), 14);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO codex_usage_event_occurrences (
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id,created_at_ms
                     ) VALUES ('codex',1,4,1,0,19,?1,1)",
                    ["a".repeat(64)],
                )
                .unwrap();
        }
        assert!(
            with_codex(&fixture.ledger, |storage| storage.commit_group(batch(
                "child",
                "root",
                source_commit(4, 14, "child", "root", 'a', false),
            )))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let occurrence_end_and_checkpoint: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT source_end_offset FROM codex_usage_event_occurrences WHERE source_file_id=4),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=4 AND consumer_kind='usage'),
                    (SELECT count(*) FROM usage_events
                        WHERE source='codex' AND source_epoch=1 AND event_id=?1)",
                ["a".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(occurrence_end_and_checkpoint, (19, 0, 1));
        drop(connection);

        fixture.add_source(5, Some("child"), 15);
        fixture.add_source(6, Some("child"), 16);
        let first_in_group = source_commit(5, 15, "child", "root", 'e', false);
        let mut stale_second = source_commit(6, 16, "child", "root", 'f', false);
        stale_second.expected_file_generation = 2;
        let atomic_group = UsageCommitBatch {
            ledger_epoch: 1,
            usage_parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            thread_id: "child".to_owned(),
            root_session_id: "root".to_owned(),
            sources: vec![first_in_group, stale_second],
        };
        assert!(
            with_codex(&fixture.ledger, |storage| storage
                .commit_group(atomic_group.clone()))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let group_state: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events WHERE event_id=?1),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=5 AND consumer_kind='usage'),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=6 AND consumer_kind='usage')",
                ["e".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(group_state, (0, 0, 0));

        // A checkpoint CAS failure happens after canonical, occurrence and
        // private state writes.  Force that failure and prove the legacy
        // Codex commit bridge rolls the entire IMMEDIATE transaction back.
        drop(connection);
        fixture.add_source(7, Some("child"), 17);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute_batch(
                    "CREATE TEMP TRIGGER force_usage_checkpoint_cas
                     BEFORE UPDATE OF committed_offset ON codex_source_checkpoints
                     WHEN OLD.source_file_id=7 AND OLD.consumer_kind='usage'
                     BEGIN SELECT RAISE(IGNORE); END",
                )
                .unwrap();
        }
        assert!(
            with_codex(&fixture.ledger, |storage| storage.commit_group(batch(
                "child",
                "root",
                source_commit(7, 17, "child", "root", 'g', false),
            )))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let checkpoint_rollback: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events
                        WHERE source='codex' AND source_epoch=1 AND event_id=?1),
                    (SELECT count(*) FROM codex_usage_event_occurrences
                        WHERE source='codex' AND ledger_epoch=1 AND source_file_id=7),
                    (SELECT count(*) FROM codex_usage_source_states
                        WHERE ledger_epoch=1 AND source_file_id=7),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=7 AND consumer_kind='usage')",
                ["g".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(checkpoint_rollback, (0, 0, 0, 0));
    }

    #[test]
    fn t_mu03_c02_durable_effort_round_trip_restart_and_fingerprint() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        let mut committed = source_commit(1, 11, "child", "root", 'e', true);
        committed.events[0].reasoning_effort = Some("high".to_owned());
        committed.updated_state.active_reasoning_effort = Some("high".to_owned());
        committed.updated_state.active_reasoning_effort_offset = Some(10);
        let turn = committed.turns.first_mut().unwrap();
        turn.reasoning_effort_state = UsageTurnReasoningEffortState::Single("high".to_owned());
        turn.unresolved_reasoning_effort_seen = false;
        if let Err(error) = with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", committed))
        }) {
            panic!(
                "durable effort commit failed: {} ({:?})",
                error,
                std::error::Error::source(&error)
            );
        }

        let scan = with_codex(&fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        let state = scan.plans[0].state.as_ref().unwrap();
        assert_eq!(state.active_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(state.active_reasoning_effort_offset, Some(10));
        let open_turn = scan.plans[0].open_turn.as_ref().unwrap();
        assert_eq!(
            open_turn.reasoning_effort_state,
            crate::codex::ingestion::usage_processor::TurnReasoningEffortState::Single(
                "high".to_owned()
            )
        );
        assert!(!open_turn.unresolved_reasoning_effort_seen);

        let fingerprint_before = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let value =
                crate::codex::storage::rebuild::active_state_fingerprint(&transaction, 1, 1)
                    .unwrap()
                    .unwrap();
            transaction.commit().unwrap();
            value
        };
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states
                     SET active_reasoning_effort='medium',active_reasoning_effort_offset=11
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        let fingerprint_after = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let value =
                crate::codex::storage::rebuild::active_state_fingerprint(&transaction, 1, 1)
                    .unwrap()
                    .unwrap();
            transaction.commit().unwrap();
            value
        };
        assert_ne!(fingerprint_before, fingerprint_after);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states
                     SET active_reasoning_effort='high',active_reasoning_effort_offset=10
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }

        let reopened = Ledger::open(LedgerOptions::new(fixture.root.join("mu.sqlite3"))).unwrap();
        let restarted = with_codex(&reopened, |storage| {
            storage.load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        let restarted_state = restarted.plans[0].state.as_ref().unwrap();
        assert_eq!(
            restarted_state.active_reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(restarted_state.active_reasoning_effort_offset, Some(10));
        assert_eq!(
            restarted.plans[0]
                .open_turn
                .as_ref()
                .unwrap()
                .reasoning_effort_state,
            crate::codex::ingestion::usage_processor::TurnReasoningEffortState::Single(
                "high".to_owned()
            )
        );
        let connection = reopened.connection().unwrap();
        let persisted: (Option<String>, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT reasoning_effort,estimated_cost_nanos_usd,
                        (SELECT unresolved_reasoning_effort_seen FROM codex_turns
                         WHERE ledger_epoch=1 AND source_file_id=1 AND turn_key='turn')
                 FROM usage_events
                 WHERE source='codex' AND source_epoch=1 AND event_id=?1",
                ["e".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(persisted, (Some("high".to_owned()), None, Some(0)));
    }

    #[test]
    fn t_hf_re01_write_turn_reasoning_effort_monotonic_matrix() {
        let fixture = Fixture::new();
        let cases = [
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::None,
                true,
            ),
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                true,
            ),
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::None,
                false,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Single("medium".to_owned()),
                false,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::None,
                false,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                false,
            ),
        ];

        for (index, (existing_state, incoming_state, allowed)) in cases.into_iter().enumerate() {
            let source_file_id = i64::try_from(index + 1).unwrap();
            fixture.add_source(source_file_id, Some("child"), source_file_id + 10);
            let mut existing = source_commit(
                source_file_id,
                source_file_id + 10,
                "child",
                "root",
                'a',
                true,
            )
            .turns
            .into_iter()
            .next()
            .unwrap();
            existing.reasoning_effort_state = existing_state;
            let mut incoming = existing.clone();
            incoming.reasoning_effort_state = incoming_state.clone();

            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            write_turn(&transaction, 1, source_file_id, 1, "child", &existing).unwrap();
            let before = durable_turn_snapshot(&transaction, source_file_id);
            let result = write_turn(&transaction, 1, source_file_id, 1, "child", &incoming);
            if allowed {
                result.unwrap();
                let after = durable_turn_snapshot(&transaction, source_file_id);
                assert_eq!(after.0, incoming.reasoning_effort_state.as_str().to_owned());
                assert_eq!(
                    after.1,
                    incoming
                        .reasoning_effort_state
                        .single_effort()
                        .map(str::to_owned)
                );
            } else {
                let error = result.unwrap_err();
                assert!(error.requires_usage_rebuild());
                assert_eq!(durable_turn_snapshot(&transaction, source_file_id), before);
            }
            transaction.commit().unwrap();
        }
    }

    #[test]
    fn t_hf_re02_same_open_turn_none_to_single_high_across_commits() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let first = source_commit(1, 11, "child", "root", 'a', true);
        let first_state = first.updated_state.clone();
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", first))
        })
        .unwrap();
        let first_snapshot = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let snapshot = durable_turn_snapshot(&transaction, 1);
            transaction.commit().unwrap();
            snapshot
        };
        assert_eq!(first_snapshot, ("none".to_owned(), None, 14, 1, 20, 10));

        let mut second = source_commit(1, 11, "child", "root", 'b', true);
        let usage = NormalizedTokenUsage::new(20, 4, Some(5), 8, 2, 28).unwrap();
        continuation_source(
            &mut second,
            first_state,
            20,
            vec![9; 32],
            40,
            vec![10; 32],
            2,
            usage,
            UsageTurnReasoningEffortState::Single("high".to_owned()),
            false,
        );
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", second))
        })
        .unwrap();

        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let final_snapshot = durable_turn_snapshot(&transaction, 1);
        assert_eq!(
            final_snapshot,
            ("single".to_owned(), Some("high".to_owned()), 28, 2, 40, 40)
        );
        let checkpoint_and_state: (i64, i64) = transaction
            .query_row(
                "SELECT
                    (SELECT committed_offset FROM codex_source_checkpoints
                     WHERE source_file_id=1 AND consumer_kind='usage'),
                    (SELECT resolved_through_offset FROM codex_usage_source_states
                     WHERE ledger_epoch=1 AND source_file_id=1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(checkpoint_and_state, (40, 40));
        transaction.commit().unwrap();
    }

    #[test]
    fn t_hf_re03_same_open_turn_none_single_high_mixed_across_commits() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let first = source_commit(1, 11, "child", "root", 'a', true);
        let first_state = first.updated_state.clone();
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", first))
        })
        .unwrap();

        let mut second = source_commit(1, 11, "child", "root", 'b', true);
        continuation_source(
            &mut second,
            first_state,
            20,
            vec![9; 32],
            40,
            vec![10; 32],
            2,
            NormalizedTokenUsage::new(20, 4, Some(5), 8, 2, 28).unwrap(),
            UsageTurnReasoningEffortState::Single("high".to_owned()),
            false,
        );
        let second_state = second.updated_state.clone();
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", second))
        })
        .unwrap();
        let second_scan = with_codex(&fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        assert!(
            !second_scan.plans[0]
                .open_turn
                .as_ref()
                .unwrap()
                .unresolved_reasoning_effort_seen
        );

        let mut third = source_commit(1, 11, "child", "root", 'c', true);
        continuation_source(
            &mut third,
            second_state,
            40,
            vec![10; 32],
            60,
            vec![11; 32],
            3,
            NormalizedTokenUsage::new(30, 6, Some(7), 12, 3, 42).unwrap(),
            UsageTurnReasoningEffortState::Mixed,
            true,
        );
        third.events[0].reasoning_effort = Some("medium".to_owned());
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", third))
        })
        .unwrap();

        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let final_snapshot = durable_turn_snapshot(&transaction, 1);
        assert_eq!(final_snapshot, ("mixed".to_owned(), None, 42, 3, 60, 60));
        let unresolved: i64 = transaction
            .query_row(
                "SELECT unresolved_reasoning_effort_seen FROM codex_turns
                 WHERE ledger_epoch=1 AND source_file_id=1 AND turn_key='turn'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unresolved, 1);
        let compensation_count: i64 = transaction
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE source='codex' AND source_epoch=1 AND event_kind='turn_compensation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if compensation_count > 0 {
            let compensation_effort: Option<String> = transaction
                .query_row(
                    "SELECT reasoning_effort FROM usage_events
                     WHERE source='codex' AND source_epoch=1 AND event_kind='turn_compensation'
                     LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(compensation_effort, None);
        }
        transaction.commit().unwrap();
    }

    #[test]
    fn t_mu03_b04_carry_preserves_cost_and_identity_ignores_derived_cost() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("child"), 12);
        let mut first_source = source_commit(1, 11, "child", "root", 'a', true);
        first_source.events[0].reasoning_effort = Some("high".to_owned());
        first_source.events[0].estimated_cost_nanos_usd = Some(5_725_000);
        first_source.updated_state.active_reasoning_effort = Some("high".to_owned());
        first_source.updated_state.active_reasoning_effort_offset = Some(10);
        first_source.turns[0].reasoning_effort_state =
            UsageTurnReasoningEffortState::Single("high".to_owned());
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", first_source))
        })
        .unwrap();
        let mut duplicate_source = source_commit(2, 12, "child", "root", 'a', true);
        duplicate_source.events[0].reasoning_effort = Some("high".to_owned());
        duplicate_source.events[0].estimated_cost_nanos_usd = None;
        duplicate_source.updated_state.active_reasoning_effort = Some("high".to_owned());
        duplicate_source
            .updated_state
            .active_reasoning_effort_offset = Some(10);
        duplicate_source.turns[0].reasoning_effort_state =
            UsageTurnReasoningEffortState::Single("high".to_owned());
        duplicate_source.anomalies[0].anomaly_id = "c".repeat(64);
        let duplicate = match with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", duplicate_source))
        }) {
            Ok(value) => value,
            Err(error) => panic!(
                "carry duplicate seed commit failed: {} ({:?})",
                error,
                std::error::Error::source(&error)
            ),
        };
        assert_eq!(
            (duplicate.events_inserted, duplicate.events_deduplicated),
            (0, 1)
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET observed_size=20 WHERE source_file_id IN (1,2)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id IN (1,2)",
                    [],
                )
                .unwrap();
        }
        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                .begin_or_resume(
                    crate::codex::normalization::USAGE_PARSER_VERSION,
                    &[1, 2],
                    30,
                )
                .unwrap();
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=2",
                    [],
                )
                .unwrap();
        }
        with_codex(&fixture.ledger, |storage| storage.begin_carry(2, 31)).unwrap();
        with_codex(&fixture.ledger, |storage| storage.resume_carry(2, 32)).unwrap();
        with_codex(&fixture.ledger, |storage| storage.resume_carry(2, 33)).unwrap();
        let connection = fixture.ledger.connection().unwrap();
        let proof: (i64, i64, String, String, Option<String>, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events
                     WHERE source='codex' AND source_epoch=2 AND event_id=?1),
                    (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=2 AND event_id=?1),
                    (SELECT carry_phase FROM codex_usage_build_sources WHERE build_epoch=2 AND source_file_id=2),
                    (SELECT reasoning_effort_state FROM codex_turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn'),
                    (SELECT single_reasoning_effort FROM codex_turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn'),
                    (SELECT unresolved_reasoning_effort_seen FROM codex_turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn')",
                ["a".repeat(64)],
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
            proof,
            (
                1,
                1,
                "anomalies".into(),
                "single".into(),
                Some("high".into()),
                0
            )
        );
        let copied_effort: Option<String> = connection
            .query_row(
                "SELECT reasoning_effort FROM usage_events
                 WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                ["a".repeat(64)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copied_effort.as_deref(), Some("high"));
        let copied_cost: Option<i64> = connection
            .query_row(
                "SELECT estimated_cost_nanos_usd FROM usage_events
                 WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                ["a".repeat(64)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copied_cost, Some(5_725_000));
    }

    #[test]
    fn storage_rejects_contradictory_fixed_view_tail_proofs_without_checkpoint_progress() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let mut exhausted_unverified = source_commit(1, 11, "child", "root", 'x', false);
        exhausted_unverified.fixed_view_exhausted = true;
        let invalid = batch("child", "root", exhausted_unverified);
        assert!(
            with_codex(&fixture.ledger, |storage| storage
                .commit_group(invalid.clone()))
            .is_err()
        );

        let mut early_none = source_commit(1, 11, "child", "root", 'y', false);
        early_none.fixed_view_exhausted = true;
        early_none.tail_status = UsageTailStatus::None;
        early_none.updated_state.raw_tail_status = UsageTailStatus::None;
        assert!(
            with_codex(&fixture.ledger, |storage| storage
                .commit_group(batch("child", "root", early_none)))
            .is_err()
        );

        let connection = fixture.ledger.connection().unwrap();
        let proof: (i64, i64) = connection
            .query_row(
                "SELECT c.committed_offset,
                        (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=1 AND source_file_id=1)
                 FROM codex_source_checkpoints c
                 WHERE c.source_file_id=1 AND c.consumer_kind='usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(proof, (0, 0));
    }

    #[test]
    fn plan_and_verified_error_recovery_matrix_preserves_metadata_boundary() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("unresolved"), 12);
        let initial = with_codex(&fixture.ledger, |storage| {
            storage
                .load_usage_scan_state(&[2, 1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        assert_eq!(initial.plans[0].action, UsagePlanAction::ReadFrom);
        assert_eq!(
            initial.plans[1].action,
            UsagePlanAction::BlockedRelationship
        );

        let first = batch(
            "child",
            "root",
            source_commit(1, 11, "child", "root", 'a', false),
        );
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(first.clone())
        })
        .unwrap();
        let resumed = with_codex(&fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        assert_eq!(resumed.plans[0].action, UsagePlanAction::ResumeOwningLive);
        assert_eq!(resumed.plans[0].start_offset, 20);
        assert_eq!(
            with_codex(&fixture.ledger, |storage| storage.load_usage_scan_state(
                &[1],
                crate::codex::normalization::USAGE_PARSER_VERSION + 1
            ))
            .unwrap()
            .plans[0]
                .action,
            UsagePlanAction::RebuildRequired
        );

        let expected_state = resumed.plans[0].state.clone().unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_checkpoints SET processing_status='error',last_error_code='USAGE_FAILED'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
        }
        let mut recovery = source_commit(1, 11, "child", "root", 'c', false);
        recovery.expected_checkpoint = UsageCheckpointExpectation {
            parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            committed_offset: 20,
            guard_hash: Some(vec![9; 32]),
            processing_status: CheckpointProcessingStatus::Error,
        };
        recovery.expected_state = Some(expected_state);
        recovery.batch_start_offset = 20;
        recovery.last_complete_offset = 40;
        recovery.source_bytes_consumed = 20;
        recovery.occurrences[0].source_start_offset = 20;
        recovery.occurrences[0].source_end_offset = 40;
        recovery.updated_state.resolved_through_offset = 40;
        recovery.updated_state.previous_total_offset = Some(40);
        let outcome = with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", recovery))
        })
        .unwrap();
        assert_eq!(outcome.data_revision, 2);
        let connection = fixture.ledger.connection().unwrap();
        let boundaries: (i64, i64, String) = connection
            .query_row(
                "SELECT
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='metadata'),
                    (SELECT committed_offset FROM codex_source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='usage'),
                    (SELECT processing_status FROM codex_source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='usage')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundaries, (80, 40, "ready".to_owned()));
        drop(connection);

        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE threads SET parent_thread_id='root',root_session_id='root',
                        agent_role='subagent' WHERE thread_id='unresolved'",
                    [],
                )
                .unwrap();
        }
        assert_eq!(
            with_codex(&fixture.ledger, |storage| storage.load_usage_scan_state(
                &[2],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ))
            .unwrap()
            .plans[0]
                .action,
            UsagePlanAction::ReadFrom
        );
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "unresolved",
                "root",
                source_commit(2, 12, "unresolved", "root", 'd', false),
            ))
        })
        .unwrap();
        assert_eq!(
            with_codex(&fixture.ledger, |storage| storage.load_usage_scan_state(
                &[2],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ))
            .unwrap()
            .plans[0]
                .action,
            UsagePlanAction::ResumeOwningLive
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET file_generation=2 WHERE source_file_id=2",
                    [],
                )
                .unwrap();
        }
        assert_eq!(
            with_codex(&fixture.ledger, |storage| storage.load_usage_scan_state(
                &[2],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ))
            .unwrap()
            .plans[0]
                .action,
            UsagePlanAction::RebuildRequired
        );
    }

    #[test]
    fn thread_groups_isolate_failures_and_root_reconcile_is_atomic_without_build() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("other-root"), 12);
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
        })
        .unwrap();
        let mut stale = source_commit(2, 12, "other-root", "other-root", 'c', false);
        stale.expected_file_generation = 2;
        assert!(
            with_codex(&fixture.ledger, |storage| storage.commit_group(batch(
                "other-root",
                "other-root",
                stale
            )))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(connection);

        with_codex(&fixture.ledger, |storage| {
            let mut transaction = storage.begin_write_txn()?;
            transaction.with_private_state(|connection| {
                connection
                    .execute(
                        "UPDATE threads SET root_session_id='other-root' WHERE thread_id='child'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(CodexStorageError::from)
            })?;
            reconcile_usage_metadata_change(
                &mut transaction,
                "child",
                Some("root"),
                Some("other-root"),
                &[],
            )
            .map_err(CodexStorageError::from)?;
            transaction.commit()
        })
        .unwrap();
        let connection = fixture.ledger.connection().unwrap();
        let roots: (String, String) = connection
            .query_row(
                "SELECT
                    (SELECT root_session_id FROM usage_events WHERE thread_id='child'),
                    (SELECT root_session_id FROM codex_usage_source_states WHERE owning_thread_id='child')",
                [],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )
            .unwrap();
        assert_eq!(roots, ("other-root".to_owned(), "other-root".to_owned()));
        drop(connection);

        fixture.add_source(3, Some("child"), 13);
        fixture
            .ledger
            .connection()
            .unwrap()
            .execute(
                "UPDATE codex_adapter_state
                 SET home_fingerprint='changed',binding_status='source_changed'
                 WHERE id=1",
                [],
            )
            .unwrap();
        assert!(
            with_codex(&fixture.ledger, |storage| storage.commit_group(batch(
                "child",
                "other-root",
                source_commit(3, 13, "child", "other-root", 'e', false),
            )))
            .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT committed_offset FROM codex_source_checkpoints
                     WHERE source_file_id=3 AND consumer_kind='usage'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn root_reconcile_requires_a_materialized_active_usage_root() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
        })
        .unwrap();

        let result: Result<(), CodexStorageError> = with_codex(&fixture.ledger, |storage| {
            let mut transaction = storage.begin_write_txn()?;
            transaction.with_private_state(|connection| {
                connection
                    .execute(
                        "UPDATE threads SET root_session_id='root-not-yet-seen'
                         WHERE thread_id='child'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(CodexStorageError::from)
            })?;
            reconcile_usage_metadata_change(
                &mut transaction,
                "child",
                Some("root"),
                Some("root-not-yet-seen"),
                &[],
            )
            .map_err(CodexStorageError::from)?;
            transaction.commit()
        });

        assert!(matches!(
            result,
            Err(CodexStorageError::Storage(error))
                if error.kind() == crate::storage::StorageErrorKind::InvalidState
        ));
        let connection = fixture.ledger.connection().unwrap();
        let roots: (String, String) = connection
            .query_row(
                "SELECT
                    (SELECT root_session_id FROM threads WHERE thread_id='child'),
                    (SELECT root_session_id FROM usage_events WHERE thread_id='child')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(roots, ("root".to_owned(), "root".to_owned()));
    }

    #[test]
    fn root_reconcile_with_build_replaces_only_affected_source_and_preserves_other_progress() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("other-root"), 12);
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
        })
        .unwrap();
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "other-root",
                "other-root",
                source_commit(2, 12, "other-root", "other-root", 'c', false),
            ))
        })
        .unwrap();

        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                .begin_or_resume(
                    crate::codex::normalization::USAGE_PARSER_VERSION,
                    &[1, 2],
                    20,
                )
                .unwrap();
            crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                .record_progress(crate::codex::storage::rebuild::SourceProgress {
                    source_file_id: 2,
                    expected_generation: 1,
                    start_offset: 0,
                    last_complete_offset: 100,
                    observed_raw_size: 100,
                    expected_guard_hash: None,
                    guard_hash: Some(vec![4; 32]),
                    tail: crate::codex::storage::rebuild::TailProof::None,
                    updated_at_ms: 21,
                })
                .unwrap();
        }
        let before_other: (String, i64, String, i64, String) = {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .query_row(
                    "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                            c.committed_offset,b.raw_tail_status
                     FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=2 AND b.source_file_id=2",
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
                .unwrap()
        };

        with_codex(&fixture.ledger, |storage| {
            let mut transaction = storage.begin_write_txn()?;
            transaction.with_private_state(|connection| {
                connection
                    .execute(
                        "UPDATE threads SET parent_thread_id='other-root',root_session_id='other-root'
                         WHERE thread_id='child'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(CodexStorageError::from)
            })?;
            reconcile_usage_metadata_change(
                &mut transaction,
                "child",
                Some("root"),
                Some("other-root"),
                &[],
            )
            .map_err(CodexStorageError::from)?;
            transaction.commit()
        })
        .unwrap();

        let connection = fixture.ledger.connection().unwrap();
        let active_roots: (String, String) = connection
            .query_row(
                "SELECT
                    (SELECT root_session_id FROM usage_events
                     WHERE source='codex' AND source_epoch=1 AND thread_id='child'),
                    (SELECT root_session_id FROM codex_usage_source_states WHERE ledger_epoch=1 AND owning_thread_id='child')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(active_roots, ("other-root".into(), "other-root".into()));
        let affected: (Option<String>, String, i64) = connection
            .query_row(
                "SELECT b.expected_root_session_id,c.processing_status,c.committed_offset
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            affected,
            (Some("other-root".into()), "rebuild_required".into(), 0)
        );
        let after_other: (String, i64, String, i64, String) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=2",
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
        assert_eq!(after_other, before_other);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM codex_usage_build_sources WHERE build_epoch=2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn strict_resume_local_replay_and_build_planner_matrix() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
        })
        .unwrap();

        let assert_action = |fixture: &Fixture, expected: UsagePlanAction| {
            let plan = with_codex(&fixture.ledger, |storage| {
                storage
                    .load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
            })
            .unwrap()
            .plans
            .into_iter()
            .next()
            .unwrap();
            assert_eq!(plan.action, expected);
        };
        assert_action(&fixture, UsagePlanAction::ResumeOwningLive);

        // Every persisted proof used by a non-zero resume is strict. Corrupt
        // one dimension at a time and prove the planner will not reuse it.
        for (column, bad, good) in [
            ("device_id", "12", "11"),
            ("inode", "12", "11"),
            ("canonical_algorithm_version", "1", "5"),
            ("resolved_through_offset", "21", "20"),
        ] {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    &format!(
                        "UPDATE codex_usage_source_states SET {column}={bad} WHERE ledger_epoch=1 AND source_file_id=1"
                    ),
                    [],
                )
                .unwrap();
            drop(connection);
            assert_action(&fixture, UsagePlanAction::RebuildRequired);
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    &format!(
                        "UPDATE codex_usage_source_states SET {column}={good} WHERE ledger_epoch=1 AND source_file_id=1"
                    ),
                    [],
                )
                .unwrap();
        }

        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET root_session_id='other-root' WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET root_session_id='root',active_turn_key='missing-turn' WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert!(
            with_codex(&fixture.ledger, |storage| storage.load_usage_scan_state(
                &[1],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ))
            .is_err()
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET active_turn_key=NULL WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_source_checkpoints SET processing_status='error',last_error_code='USAGE_FAILED' WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::ResumeOwningLive);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_checkpoints SET guard_hash=?1 WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![1_u8; 31]],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);

        // LocalReplay is allowed only under the same active identity/parser/
        // ownership/canonical proof. A physical identity change promotes the
        // source to a whole-ledger rebuild instead of replaying in place.
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_checkpoints SET processing_status='rebuild_required',committed_offset=20,guard_hash=?1,last_error_code=NULL WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![9_u8; 32]],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::LocalReplay);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET inode=99 WHERE source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);

        // A shadow build never applies LocalReplaySafe. It starts from zero,
        // can persist a bounded intermediate batch, and resumes BuildFrom at
        // the committed complete-line boundary.
        let build_fixture = Fixture::new();
        build_fixture.add_source(2, Some("child"), 22);
        {
            let mut connection = build_fixture.ledger.connection().unwrap();
            let snapshot =
                crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                    .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[2], 20)
                    .unwrap();
            assert_eq!(snapshot.build_epoch, 2);
        }
        let first_plan = with_codex(&build_fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[2], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap()
        .plans
        .remove(0);
        assert_eq!(first_plan.action, UsagePlanAction::BuildFrom);
        assert_eq!(first_plan.start_offset, 0);

        let mut build_commit = source_commit(2, 22, "child", "root", 'b', false);
        build_commit.expected_checkpoint.processing_status =
            CheckpointProcessingStatus::RebuildRequired;
        build_commit.fixed_observed_raw_size = 100;
        let build_batch = UsageCommitBatch {
            ledger_epoch: 2,
            usage_parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            thread_id: "child".to_owned(),
            root_session_id: "root".to_owned(),
            sources: vec![build_commit],
        };
        with_codex(&build_fixture.ledger, |storage| {
            storage.commit_group(build_batch.clone())
        })
        .unwrap();
        let continued = with_codex(&build_fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[2], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap()
        .plans
        .remove(0);
        assert_eq!(continued.action, UsagePlanAction::BuildFrom);
        assert_eq!(continued.start_offset, 20);
    }

    #[test]
    fn persistent_partial_seed_carry_is_atomic_restarts_from_first_key_and_rejects_seed_conflict() {
        fn prepare() -> Fixture {
            let fixture = Fixture::new();
            fixture.add_source(1, Some("child"), 11);
            with_codex(&fixture.ledger, |storage| {
                storage.commit_group(batch(
                    "child",
                    "root",
                    source_commit(1, 11, "child", "root", 'a', true),
                ))
            })
            .unwrap();
            {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE codex_source_files SET observed_size=20 WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE codex_usage_source_states SET observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL WHERE ledger_epoch=1 AND source_file_id=1",
                        [],
                    )
                    .unwrap();
            }
            {
                let mut connection = fixture.ledger.connection().unwrap();
                crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                    .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[1], 30)
                    .unwrap();
            }
            let mut seed = source_commit(1, 11, "child", "root", 'a', true);
            seed.expected_checkpoint.processing_status =
                CheckpointProcessingStatus::RebuildRequired;
            seed.fixed_observed_raw_size = 20;
            seed.updated_state.observed_raw_size = 20;
            let seed_batch = UsageCommitBatch {
                ledger_epoch: 2,
                usage_parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
                thread_id: "child".to_owned(),
                root_session_id: "root".to_owned(),
                sources: vec![seed],
            };
            with_codex(&fixture.ledger, |storage| {
                storage.commit_group(seed_batch.clone())
            })
            .unwrap();
            {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
            }
            {
                let scan = with_codex(&fixture.ledger, |storage| {
                    storage.load_usage_scan_state(
                        &[1],
                        crate::codex::normalization::USAGE_PARSER_VERSION,
                    )
                })
                .unwrap();
                assert_eq!(scan.plans[0].action, UsagePlanAction::BeginCarry);
            }
            fixture
        }

        // Failure while flipping the manifest phase must roll back the state
        // retirement and checkpoint reset from the same BeginCarry transaction.
        let fixture = prepare();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_begin_carry BEFORE UPDATE OF carry_phase ON codex_usage_build_sources
                     WHEN NEW.carry_phase='occurrences'
                     BEGIN SELECT RAISE(ABORT,'injected carry failure'); END;",
                )
                .unwrap();
        }
        assert!(with_codex(&fixture.ledger, |storage| storage.begin_carry(1, 40)).is_err());
        {
            let connection = fixture.ledger.connection().unwrap();
            let proof: (String, i64, i64, String) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,
                            (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=2 AND source_file_id=1),
                            b.carry_phase
                     FROM codex_source_checkpoints c JOIN codex_usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(proof, ("ready".into(), 20, 1, "none".into()));
            connection
                .execute_batch("DROP TRIGGER fail_begin_carry;")
                .unwrap();
        }

        with_codex(&fixture.ledger, |storage| storage.begin_carry(1, 41)).unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            let proof: (String, i64, i64, String, i64, i64, i64) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,
                            (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=2 AND source_file_id=1),
                            b.carry_phase,
                            (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=1),
                            (SELECT count(*) FROM codex_turns WHERE ledger_epoch=2 AND source_file_id=1),
                            (SELECT count(*) FROM codex_ingest_anomalies WHERE ledger_epoch=2 AND source_file_id=1)
                     FROM codex_source_checkpoints c JOIN codex_usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                )
                .unwrap();
            assert_eq!(
                proof,
                (
                    "rebuild_required".into(),
                    0,
                    0,
                    "occurrences".into(),
                    1,
                    1,
                    1
                )
            );
        }
        for _ in 0..8 {
            let outcome =
                with_codex(&fixture.ledger, |storage| storage.resume_carry(1, 50)).unwrap();
            if outcome == CarryStepOutcome::FinalizedMissing {
                break;
            }
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            let final_proof: (String, i64, String, String, i64) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,b.completion_status,b.carry_phase,
                            (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=2 AND source_file_id=1)
                     FROM codex_source_checkpoints c JOIN codex_usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .unwrap();
            assert_eq!(
                final_proof,
                ("ready".into(), 20, "carried".into(), "none".into(), 1)
            );
        }

        // A partial seed is not trusted merely because its key already exists.
        // Resume enumerates active facts from the first key and hard-fails on
        // incompatible seed payload without advancing the durable cursor.
        let conflict = prepare();
        with_codex(&conflict.ledger, |storage| storage.begin_carry(1, 60)).unwrap();
        {
            let connection = conflict.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_events SET model='seed-conflict'
                     WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                    ["a".repeat(64)],
                )
                .unwrap();
        }
        assert!(with_codex(&conflict.ledger, |storage| storage.resume_carry(1, 61)).is_err());
        let connection = conflict.ledger.connection().unwrap();
        let unchanged: (String, Option<i64>, String, i64) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,c.processing_status,c.committed_offset
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                 WHERE b.build_epoch=2 AND b.source_file_id=1 AND c.consumer_kind='usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            unchanged,
            ("occurrences".into(), None, "rebuild_required".into(), 0)
        );

        // A partial seed may not smuggle a cross-source-provenance orphan
        // canonical event into the build epoch. BeginCarry rejects it before
        // retiring the seed state.
        let orphan = prepare();
        orphan.add_source(2, Some("child"), 22);
        {
            let connection = orphan.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                     SELECT source,source_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms
                     FROM usage_events
                     WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "orphan"],
                )
                .unwrap();
        }
        assert!(with_codex(&orphan.ledger, |storage| storage.begin_carry(1, 62)).is_err());
        let connection = orphan.ledger.connection().unwrap();
        let unchanged: (String, i64, String, i64) = connection
            .query_row(
                "SELECT c.processing_status,c.committed_offset,b.carry_phase,
                        (SELECT count(*) FROM codex_usage_source_states
                         WHERE ledger_epoch=2 AND source_file_id=1)
                 FROM codex_source_checkpoints c JOIN codex_usage_build_sources b
                   ON b.source_file_id=c.source_file_id
                 WHERE c.source_file_id=1 AND c.consumer_kind='usage'
                   AND b.build_epoch=2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(unchanged, ("ready".into(), 20, "none".into(), 1));

        // The same protection applies if a cross-source-provenance orphan is
        // introduced after the carry cursor starts: finalization remains
        // blocked and atomic.
        let finalize_orphan = prepare();
        with_codex(&finalize_orphan.ledger, |storage| {
            storage.begin_carry(1, 65)
        })
        .unwrap();
        finalize_orphan.add_source(2, Some("child"), 22);
        {
            let connection = finalize_orphan.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                     SELECT source,source_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms
                     FROM usage_events
                     WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "late-orphan"],
                )
                .unwrap();
        }
        for step in 0..3_i64 {
            assert!(matches!(
                with_codex(&finalize_orphan.ledger, |storage| storage
                    .resume_carry(1, 66 + step)),
                Ok(CarryStepOutcome::Progress)
            ));
        }
        assert!(
            with_codex(&finalize_orphan.ledger, |storage| storage
                .resume_carry(1, 69))
            .is_err()
        );
        let connection = finalize_orphan.ledger.connection().unwrap();
        let blocked: (
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
            String,
            i64,
        ) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,b.carry_after_turn_key,
                        b.carry_after_anomaly_id,c.committed_offset,c.processing_status,
                         (SELECT count(*) FROM usage_events
                         WHERE source='codex' AND source_epoch=2 AND event_id='late-orphan')
                 FROM codex_usage_build_sources b
                 JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
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
            blocked,
            (
                "finalize".into(),
                None,
                None,
                None,
                0,
                "rebuild_required".into(),
                1,
            )
        );

        // A same-offset occurrence with a different event id is a durable
        // occurrence conflict, not a seed that can be silently overwritten.
        let occurrence_conflict = prepare();
        with_codex(&occurrence_conflict.ledger, |storage| {
            storage.begin_carry(1, 63)
        })
        .unwrap();
        {
            let connection = occurrence_conflict.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                     SELECT source,source_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms
                     FROM usage_events
                     WHERE source='codex' AND source_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "wrong-event"],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_event_occurrences SET event_id='wrong-event'
                     WHERE source='codex' AND ledger_epoch=2 AND source_file_id=1 AND source_start_offset=0",
                    [],
                )
                .unwrap();
        }
        let error = with_codex(&occurrence_conflict.ledger, |storage| {
            storage.resume_carry(1, 64)
        })
        .unwrap_err();
        assert!(error.requires_rebuild());
        let connection = occurrence_conflict.ledger.connection().unwrap();
        let unchanged: (String, Option<i64>, String, i64, String) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,
                        c.processing_status,c.committed_offset,o.event_id
                 FROM codex_usage_build_sources b
                 JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 JOIN codex_usage_event_occurrences o ON o.ledger_epoch=2
                    AND o.source_file_id=1 AND o.source_start_offset=0
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
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
            unchanged,
            (
                "occurrences".into(),
                None,
                "rebuild_required".into(),
                0,
                "wrong-event".into()
            )
        );
    }

    #[test]
    fn active_unverified_tail_rejects_begin_usage_carry() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', true),
            ))
        })
        .unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET observed_size=20
                     WHERE source_file_id=1",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_source_checkpoints SET committed_offset=20,guard_hash=?1,
                        processing_status='ready'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![9_u8; 32]],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET resolved_through_offset=20,
                        observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[1], 30)
                .unwrap();
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=1;
                     UPDATE codex_source_checkpoints SET committed_offset=0,guard_hash=NULL,
                        processing_status='rebuild_required'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_source_states SET raw_tail_status='unverified',
                        raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }

        let plan = with_codex(&fixture.ledger, |storage| {
            storage.load_usage_scan_state(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
        })
        .unwrap();
        assert_eq!(plan.plans[0].action, UsagePlanAction::BlockedRelationship);
        assert!(with_codex(&fixture.ledger, |storage| storage.begin_carry(1, 40)).is_err());
    }
}
