use super::*;

use rusqlite::{Connection, TransactionBehavior, params};

fn event_id(letter: char) -> String {
    letter.to_string().repeat(64)
}

fn set_active_proof(fixture: &Fixture, source_id: i64, offset: i64) {
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET observed_size=?2,observed_mtime_ns=2
             WHERE source_file_id=?1",
            params![source_id, offset],
        )
        .unwrap();
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_checkpoints SET committed_offset=?2,guard_hash=?3,
                processing_status='ready',last_error_code=NULL
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            params![source_id, offset, vec![9_u8; 32]],
        )
        .unwrap();
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_usage_source_states SET resolved_through_offset=?2,
                observed_raw_size=?2,raw_tail_status='none',raw_tail_start_offset=NULL,
                previous_total_offset=?2,updated_at_ms=20
             WHERE ledger_epoch=1 AND source_file_id=?1",
            params![source_id, offset],
        )
        .unwrap();
}

fn clone_event(connection: &Connection, from_epoch: i64, to_epoch: i64, id: &str) {
    clone_event_as(connection, from_epoch, to_epoch, id, id);
}

fn clone_event_as(
    connection: &Connection,
    from_epoch: i64,
    to_epoch: i64,
    source_id: &str,
    target_id: &str,
) {
    connection
        .execute(
            "INSERT INTO usage_events(
                source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,
                cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,
                quality_status,created_at_ms)
             SELECT source,?1,?4,event_kind,occurred_at_ms,thread_id,root_session_id,
                turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,
                cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,
                quality_status,created_at_ms
             FROM usage_events WHERE source='codex' AND source_epoch=?2 AND event_id=?3",
            params![to_epoch, from_epoch, source_id, target_id],
        )
        .unwrap();
}

fn insert_fact(connection: &Connection, epoch: i64, id: &str, response_id: &str, operation: &str) {
    connection
        .execute(
            "INSERT INTO codex_usage_event_facts(
                source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
             VALUES ('codex',?1,?2,'child',?3,'explicit',?4)",
            params![epoch, id, response_id, operation],
        )
        .unwrap();
}

fn empty_window_json() -> String {
    crate::codex::ingestion::usage_processor::LegacyReconciliationWindow {
        version: crate::codex::ingestion::usage_processor::LegacyReconciliationWindow::VERSION,
        previous_total: crate::codex::usage::UsageValue::Missing,
        current_total: crate::codex::usage::UsageValue::Missing,
        last_usage: crate::codex::usage::UsageValue::Missing,
        explicit_response_ids: Vec::new(),
        legacy_covered_response_ids: Vec::new(),
        proposal_event_ids: Vec::new(),
        turn_accounted_before: NormalizedTokenUsage::zero(),
        chain_state: crate::codex::ingestion::usage_processor::ChainState::Continuous,
        closed: false,
    }
    .to_json()
    .unwrap()
}

fn response_reconciliation_context(
    fixture: &Fixture,
    source: &UsageSourceCommit,
    response_id: &str,
) -> Result<UsageReconciliationContext, CodexStorageError> {
    let request = crate::codex::ingestion::usage_processor::ReconciliationRequest::new(
        vec![crate::codex::ingestion::usage_processor::ResponseKey {
            owning_thread_id: source.updated_state.owning_thread_id.clone(),
            response_id: response_id.to_owned(),
        }],
        Vec::new(),
    );
    let basic_proof = UsageReconciliationBasicProof {
        device_id: source.updated_state.device_id,
        inode: source.updated_state.inode,
        observed_raw_size: source.fixed_observed_raw_size,
        expected_checkpoint: (!source.expected_checkpoint_missing)
            .then(|| source.expected_checkpoint.clone()),
        expected_state: source.expected_state.clone(),
    };
    with_codex(&fixture.ledger, |storage| {
        storage.load_usage_reconciliation_context(
            1,
            crate::codex::ingestion::usage_processor::UsageContext {
                source_file_id: source.source_file_id,
                file_generation: source.expected_file_generation,
                owning_thread_id: source.updated_state.owning_thread_id.clone(),
                root_session_id: source.updated_state.root_session_id.clone(),
            },
            request,
            basic_proof,
        )
    })
}

fn set_noop_commit_snapshot(
    fixture: &Fixture,
    source: &mut UsageSourceCommit,
    source_id: i64,
) -> (UsageCheckpointExpectation, UsageSourceStateWrite) {
    let connection = fixture.ledger.connection().unwrap();
    let checkpoint = read_usage_checkpoint(&connection, source_id)
        .unwrap()
        .unwrap();
    let state = read_usage_source_state(&connection, 1, source_id)
        .unwrap()
        .unwrap();
    source.expected_checkpoint = checkpoint.clone();
    source.expected_checkpoint_missing = false;
    source.expected_state = Some(state.clone());
    source.local_replay = false;
    source.batch_start_offset = checkpoint.committed_offset;
    source.last_complete_offset = checkpoint.committed_offset;
    source.fixed_observed_raw_size = state.observed_raw_size;
    source.source_bytes_consumed = 0;
    source.complete_line_count = 0;
    source.fixed_view_exhausted = state.raw_tail_status != UsageTailStatus::Unverified;
    source.tail_status = state.raw_tail_status;
    source.tail_start_offset = state.raw_tail_start_offset;
    source.updated_state = state.clone();
    source.next_guard_hash = checkpoint.guard_hash.clone();
    source.patch = ReconciliationPatchWrite::default();
    source.reconciliation_request =
        crate::codex::ingestion::usage_processor::ReconciliationRequest::default();
    source.reconciliation_expected_fingerprint = vec![0; 32];
    refresh_patch_counts(source);
    (checkpoint, state)
}

fn prepare_marker_window_carry() -> (Fixture, String, String) {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ),
        )
    })
    .unwrap();
    fixture.add_source(2, Some("child"), 12);
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(2, 12, "child", "root", 'a', false),
            ),
        )
    })
    .unwrap();
    set_active_proof(&fixture, 1, 20);
    set_active_proof(&fixture, 2, 20);

    let marker_event = event_id('b');
    let response_event = event_id('a');
    {
        let connection = fixture.ledger.connection().unwrap();
        clone_event_as(&connection, 1, 1, &response_event, &marker_event);
        insert_fact(
            &connection,
            1,
            &marker_event,
            "marker-response",
            "compaction",
        );
        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,30,31,'child','root',30,'model',NULL,
                         'marker-response',?1,NULL)",
                [&marker_event],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_usage_reconciliation_windows(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,turn_key,state_json)
                 VALUES ('codex',1,1,1,40,41,'child',NULL,?1)",
                [empty_window_json()],
            )
            .unwrap();
        insert_fact(
            &connection,
            1,
            &response_event,
            "ordinary-response",
            "response",
        );
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
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id IN (1,2)",
            [],
        )
        .unwrap();
    with_codex(&fixture.ledger, |storage| storage.begin_carry(1, 40)).unwrap();
    (fixture, marker_event, response_event)
}

fn carry_phase_from_db(fixture: &Fixture) -> String {
    fixture
        .ledger
        .connection()
        .unwrap()
        .query_row(
            "SELECT carry_phase FROM codex_usage_build_sources
             WHERE build_epoch=2 AND source_file_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn compaction_event_occurrence_lookup_is_sparse_with_unrelated_rows() {
    let fixture = Fixture::new();
    for source_id in 1..=6 {
        fixture.add_source(source_id, Some("child"), 10 + source_id);
    }
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ),
        )
    })
    .unwrap();

    let target_event_id = event_id('a');
    {
        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection.transaction().unwrap();
        for index in 0..24 {
            let unrelated_event_id = format!("unrelated-{index}");
            clone_event_as(&transaction, 1, 1, &target_event_id, &unrelated_event_id);
            let source_file_id = i64::try_from(index % 6 + 1).unwrap();
            let source_offset = 1 + i64::try_from(index / 6).unwrap() * 2;
            transaction
                .execute(
                    "INSERT INTO codex_usage_event_occurrences(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id,created_at_ms)
                     VALUES ('codex',1,?1,1,?2,?3,?4,20)",
                    params![source_file_id, source_offset, source_offset + 1, unrelated_event_id],
                )
                .unwrap();
        }
        for (source_file_id, source_offset) in [(1, 80), (2, 70), (3, 70)] {
            transaction
                .execute(
                    "INSERT INTO codex_usage_event_occurrences(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id,created_at_ms)
                     VALUES ('codex',1,?1,1,?2,?3,?4,20)",
                    params![source_file_id, source_offset, source_offset + 1, target_event_id],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }

    let connection = fixture.ledger.connection().unwrap();
    let occurrences = load_event_occurrences(&connection, 1, &target_event_id).unwrap();
    assert_eq!(
        occurrences
            .iter()
            .map(|row| (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
                row.source_end_offset,
            ))
            .collect::<Vec<_>>(),
        vec![(1, 1, 0, 20), (1, 1, 80, 81), (2, 1, 70, 71), (3, 1, 70, 71)]
    );

    let plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT source_file_id,file_generation,source_start_offset,
                    source_end_offset,event_id
             FROM codex_usage_event_occurrences INDEXED BY codex_usage_event_occurrences_event_idx
             WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2
             ORDER BY source_file_id,file_generation,source_start_offset",
        )
        .unwrap()
        .query_map(params![1, target_event_id], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(plan.iter().any(|detail| {
        detail.contains("codex_usage_event_occurrences_event_idx")
    }));
}

#[test]
fn compaction_state_proof() {
    for change_fact in [false, true] {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        with_codex(&fixture.ledger, |storage| {
            test_commit_group(
                storage,
                batch(
                    "child",
                    "root",
                    source_commit(1, 11, "child", "root", 'a', false),
                ),
            )
        })
        .unwrap();
        if change_fact {
            insert_fact(
                &fixture.ledger.connection().unwrap(),
                1,
                &event_id('a'),
                "response-a",
                "response",
            );
        }
        set_active_proof(&fixture, 1, 20);
        let mut stale_commit = source_commit(1, 11, "child", "root", 'b', false);
        if !change_fact {
            set_noop_commit_snapshot(&fixture, &mut stale_commit, 1);
        }
        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[1], 30)
                .unwrap();
        }

        if change_fact {
            fixture
                .ledger
                .connection()
                .unwrap()
                .execute(
                    "UPDATE codex_usage_event_facts SET operation='compaction'
                     WHERE ledger_epoch=1 AND event_id=?1",
                    [event_id('a')],
                )
                .unwrap();
        } else {
            let carry = crate::codex::ingestion::usage_processor::ReconciliationCarry {
                open_window_start_offset: Some(1),
                ..Default::default()
            }
            .to_json()
            .unwrap();
            fixture
                .ledger
                .connection()
                .unwrap()
                .execute(
                    "UPDATE codex_usage_source_states SET reconciliation_state_json=?1
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [carry],
                )
                .unwrap();
            assert!(
                with_codex(&fixture.ledger, |storage| {
                    test_commit_group(storage, batch("child", "root", stale_commit))
                })
                .is_err()
            );
        }
        fixture
            .ledger
            .connection()
            .unwrap()
            .execute(
                "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert!(with_codex(&fixture.ledger, |storage| storage.begin_carry(1, 40)).is_err());
    }
}

#[test]
fn compaction_visibility_activation() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture.add_source(2, Some("unresolved"), 12);
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', true),
            ),
        )
    })
    .unwrap();
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_turns SET ended_at_ms=5,end_offset=40,status='completed'
             WHERE ledger_epoch=1 AND source_file_id=1 AND file_generation=1 AND turn_key='turn'",
            [],
        )
        .unwrap();
    let id = event_id('a');
    let parser = crate::codex::analytics::SKILL_USAGE_PARSER_VERSION + 1;
    let before = {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE source_usage_epochs SET active_parser_version=?1,
                    build_epoch=2,build_parser_version=?1 WHERE source='codex'",
                [parser],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints SET parser_version=?1,committed_offset=100,
                    processing_status='ready' WHERE source_file_id=1 AND consumer_kind='usage'",
                [parser],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_usage_source_states SET usage_parser_version=?1,
                    observed_raw_size=100 WHERE ledger_epoch=1 AND source_file_id=1",
                [parser],
            )
            .unwrap();
        insert_fact(&connection, 1, &id, "response-a", "response");
        clone_event(&connection, 1, 2, &id);
        insert_fact(&connection, 2, &id, "response-a", "compaction");
        crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser).unwrap()
    };
    assert!(before.ready);
    assert!(
        before
            .unknown_scopes
            .iter()
            .any(|scope| scope.thread_id == "child")
    );
    assert!(
        before
            .unknown_scopes
            .iter()
            .all(|scope| scope.thread_id != "unresolved")
    );
    let rebound_signature = {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id='other-root'
                 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        let signature =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id='child' WHERE source_file_id=1",
                [],
            )
            .unwrap();
        signature
    };
    assert_eq!(before, rebound_signature);

    fixture.add_source(3, Some("other-root"), 13);
    let scope_signature = {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,3,1,0,1,'other-root','other-root',NULL,NULL,NULL,
                         NULL,NULL,'time_missing')",
                [],
            )
            .unwrap();
        let with_ghost_scope =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_eq!(before, with_ghost_scope);

        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,30,31,'child','root',NULL,NULL,NULL,
                         NULL,NULL,'time_missing')",
                [],
            )
            .unwrap();
        let initial =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_eq!(before, initial);
        connection
            .execute(
                "UPDATE codex_compaction_markers SET unknown_reason='identity_missing'
                 WHERE ledger_epoch=1 AND source_file_id=1 AND source_start_offset=30",
                [],
            )
            .unwrap();
        let changed_reason =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_eq!(initial, changed_reason);

        connection
            .execute(
                "UPDATE codex_usage_source_states SET resolved_through_offset=100,
                    raw_tail_status='none',raw_tail_start_offset=NULL
                 WHERE ledger_epoch=1 AND source_file_id=1",
                [],
            )
            .unwrap();
        let bounded =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert!(bounded.unknown_scopes.iter().any(|scope| {
            scope.thread_id == "child"
                && scope.root_session_id == "root"
                && scope.model == "model"
                && scope.start_ms == Some(1)
                && scope.end_ms == Some(6)
        }));

        connection
            .execute(
                "UPDATE codex_turns SET ended_at_ms=4
                 WHERE ledger_epoch=1 AND source_file_id=1 AND file_generation=1 AND turn_key='turn'",
                [],
            )
            .unwrap();
        let moved_turn_end =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_ne!(bounded, moved_turn_end);
        assert!(moved_turn_end.unknown_scopes.iter().any(|scope| {
            scope.thread_id == "child"
                && scope.root_session_id == "root"
                && scope.model == "model"
                && scope.start_ms == Some(1)
                && scope.end_ms == Some(5)
        }));
        connection
            .execute(
                "UPDATE codex_turns SET ended_at_ms=5
                 WHERE ledger_epoch=1 AND source_file_id=1 AND file_generation=1 AND turn_key='turn'",
                [],
            )
            .unwrap();

        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,32,33,'child','root',1000,'model',NULL,
                         NULL,NULL,'usage_missing')",
                [],
            )
            .unwrap();
        let single_non_overlapping_marker =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,33,34,'child','root',1000,'model',NULL,
                         NULL,NULL,'identity_missing')",
                [],
            )
            .unwrap();
        let non_overlapping_marker =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_eq!(single_non_overlapping_marker, non_overlapping_marker);
        assert!(non_overlapping_marker.unknown_scopes.iter().any(|scope| {
            scope.thread_id == "child"
                && scope.root_session_id == "root"
                && scope.model == "model"
                && scope.start_ms == Some(1000)
                && scope.end_ms == Some(1001)
        }));
        connection
            .execute(
                "UPDATE codex_compaction_markers SET unknown_reason='time_missing'
                 WHERE ledger_epoch=1 AND source_file_id=1 AND source_start_offset=33",
                [],
            )
            .unwrap();
        let duplicate_reason_changed =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_eq!(non_overlapping_marker, duplicate_reason_changed);

        connection
            .execute(
                "UPDATE codex_compaction_markers SET occurred_at_ms=2000
                 WHERE ledger_epoch=1 AND source_file_id=1 AND source_start_offset IN (32,33)",
                [],
            )
            .unwrap();
        let moved_marker_time =
            crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser)
                .unwrap();
        assert_ne!(non_overlapping_marker, moved_marker_time);
        assert!(moved_marker_time.unknown_scopes.iter().any(|scope| {
            scope.thread_id == "child"
                && scope.root_session_id == "root"
                && scope.model == "model"
                && scope.start_ms == Some(2000)
                && scope.end_ms == Some(2001)
        }));
        connection
            .execute(
                "DELETE FROM codex_compaction_markers
                 WHERE ledger_epoch=1 AND source_file_id=1 AND source_start_offset IN (32,33)",
                [],
            )
            .unwrap();
        crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser).unwrap()
    };

    let active_epoch_signature = {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE codex_usage_source_states SET resolved_through_offset=100,
                    raw_tail_status='none',raw_tail_start_offset=NULL
                 WHERE ledger_epoch=1 AND source_file_id=1",
                [],
            )
            .unwrap();
        crate::codex::analytics::compaction_visibility_signature(&connection, 1, parser).unwrap()
    };
    assert_eq!(active_epoch_signature, scope_signature);

    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM codex_compaction_markers
             WHERE ledger_epoch=1 AND source_file_id=1 AND source_start_offset=30",
            [],
        )
        .unwrap();

    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM codex_usage_source_states WHERE ledger_epoch=1 AND source_file_id=1",
            [],
        )
        .unwrap();
    let missing_state_signature = crate::codex::analytics::compaction_visibility_signature(
        &fixture.ledger.connection().unwrap(),
        1,
        parser,
    )
    .unwrap();
    assert!(
        missing_state_signature
            .unknown_scopes
            .iter()
            .any(|scope| scope.thread_id == "child")
    );

    let previous_revision: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let outcome = with_codex(&fixture.ledger, |storage| {
        let mut tx = storage.begin_write_txn().unwrap();
        let outcome = tx
            .activate_usage_build_with_private_visibility(
                2,
                parser,
                |connection, _, active, active_parser, build, build_parser| {
                    let active = crate::codex::analytics::compaction_visibility_signature(
                        connection,
                        active,
                        active_parser,
                    )
                    .map_err(CodexStorageError::from)?;
                    let build = crate::codex::analytics::compaction_visibility_signature(
                        connection,
                        build,
                        build_parser,
                    )
                    .map_err(CodexStorageError::from)?;
                    Ok(active == build)
                },
            )
            .unwrap();
        tx.commit().unwrap();
        outcome
    });
    assert!(outcome.visible_changed);
    assert_eq!(outcome.data_revision, previous_revision + 1);
}

#[test]
fn compaction_visibility_activation_source_observation_revision() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture.add_source(2, Some("child"), 12);
    for (source_id, device, event) in [(1, 11, 'a'), (2, 12, 'a')] {
        with_codex(&fixture.ledger, |storage| {
            test_commit_group(
                storage,
                batch(
                    "child",
                    "root",
                    source_commit(source_id, device, "child", "root", event, false),
                ),
            )
        })
        .unwrap();
        set_active_proof(&fixture, source_id, 100);
    }

    let parser = crate::codex::analytics::SKILL_USAGE_PARSER_VERSION + 1;
    {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE source_usage_epochs SET active_parser_version=?1
                 WHERE source='codex'",
                [parser],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints SET parser_version=?1
                 WHERE consumer_kind='usage' AND source_file_id IN (1,2)",
                [parser],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_usage_source_states SET usage_parser_version=?1
                 WHERE ledger_epoch=1 AND source_file_id IN (1,2)",
                [parser],
            )
            .unwrap();
        insert_fact(&connection, 1, &event_id('a'), "response-a", "compaction");
    }
    let complete_signature = crate::codex::analytics::compaction_visibility_signature(
        &fixture.ledger.connection().unwrap(),
        1,
        parser,
    )
    .unwrap();
    assert!(complete_signature.unknown_scopes.is_empty());

    let revision_before_append: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let appended = crate::codex::domain::SourceObservationBatch::new(
        vec![
            crate::codex::domain::SourceObservation::new(
                "/tmp/usage-1.jsonl",
                crate::codex::domain::SourceArea::Sessions,
                11,
                11,
                150,
                2,
                2,
            )
            .unwrap(),
        ],
        crate::codex::domain::SourceRegionStatus::Complete,
        crate::codex::domain::SourceRegionStatus::Complete,
    )
    .unwrap();
    let outcome = with_codex(&fixture.ledger, |storage| {
        storage.record_source_observations_with_usage_carry_proofs(appended, &[])
    })
    .unwrap();
    assert_eq!(outcome.results.len(), 1);
    let revision_after_append: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision_after_append, revision_before_append + 1);
    let appended_signature = crate::codex::analytics::compaction_visibility_signature(
        &fixture.ledger.connection().unwrap(),
        1,
        parser,
    )
    .unwrap();
    assert!(
        appended_signature
            .unknown_scopes
            .iter()
            .any(|scope| scope.thread_id == "child" && scope.root_session_id == "root")
    );

    // A shared checkpoint may advance to the new tail before this epoch's
    // source-state snapshot is refreshed; it cannot complete the old proof.
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_checkpoints SET committed_offset=150
             WHERE source_file_id=1 AND consumer_kind='usage'",
            [],
        )
        .unwrap();
    let checkpoint_ahead_signature = crate::codex::analytics::compaction_visibility_signature(
        &fixture.ledger.connection().unwrap(),
        1,
        parser,
    )
    .unwrap();
    assert_eq!(appended_signature, checkpoint_ahead_signature);

    let replaced = crate::codex::domain::SourceObservationBatch::new(
        vec![
            crate::codex::domain::SourceObservation::new(
                "/tmp/usage-1.jsonl",
                crate::codex::domain::SourceArea::Sessions,
                91,
                92,
                150,
                3,
                4,
            )
            .unwrap(),
        ],
        crate::codex::domain::SourceRegionStatus::Complete,
        crate::codex::domain::SourceRegionStatus::Complete,
    )
    .unwrap();
    let replaced = with_codex(&fixture.ledger, |storage| {
        storage.record_source_observations_with_usage_carry_proofs(replaced, &[])
    })
    .unwrap();
    assert!(replaced.results[0].replaced);
    let stopped_state: (i64, String, String) = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row(
            "SELECT file_generation,owning_thread_id,root_session_id
             FROM codex_usage_source_states WHERE ledger_epoch=1 AND source_file_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stopped_state, (1, "child".to_owned(), "root".to_owned()));
    let replacement_signature = crate::codex::analytics::compaction_visibility_signature(
        &fixture.ledger.connection().unwrap(),
        1,
        parser,
    )
    .unwrap();
    assert_eq!(appended_signature, replacement_signature);
    let revision_after_replacement: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision_after_replacement, revision_after_append);
}

#[test]
fn compaction_carry_phases() {
    let (fixture, marker_event, response_event) = prepare_marker_window_carry();
    let phases = [
        ("occurrences", "facts"),
        ("facts", "markers"),
        ("markers", "windows"),
        ("windows", "turns"),
        ("turns", "anomalies"),
        ("anomalies", "finalize"),
    ];
    for (phase, next) in phases {
        assert_eq!(carry_phase_from_db(&fixture), phase);
        if phase == "facts" {
            fixture
                .ledger
                .connection()
                .unwrap()
                .execute(
                    "INSERT INTO codex_usage_event_facts(
                        source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
                     VALUES ('codex',2,?1,'child','ordinary-response','explicit','compaction')",
                    [&response_event],
                )
                .unwrap();
        }
        with_codex(&fixture.ledger, |storage| storage.resume_carry(1, 50)).unwrap();
        assert_eq!(carry_phase_from_db(&fixture), next);
    }
    let carried_operation: String = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row(
            "SELECT operation FROM codex_usage_event_facts
             WHERE ledger_epoch=2 AND event_id=?1",
            [&response_event],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(carried_operation, "compaction");
    assert_eq!(carry_phase_from_db(&fixture), "finalize");
    let result = with_codex(&fixture.ledger, |storage| storage.resume_carry(1, 70)).unwrap();
    assert_eq!(result, CarryStepOutcome::FinalizedMissing);
    assert_eq!(carry_phase_from_db(&fixture), "none");

    let marker_copies: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM codex_compaction_markers
             WHERE ledger_epoch=2 AND source_file_id=1 AND resolved_event_id=?1",
            [&marker_event],
            |row| row.get(0),
        )
        .unwrap();
    let window_copies: i64 = fixture
        .ledger
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM codex_usage_reconciliation_windows
             WHERE ledger_epoch=2 AND source_file_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((marker_copies, window_copies), (1, 1));
}

#[test]
fn compaction_replay_orphan_hold() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ),
        )
    })
    .unwrap();
    fixture.add_source(2, Some("child"), 12);
    with_codex(&fixture.ledger, |storage| {
        test_commit_group(
            storage,
            batch(
                "child",
                "root",
                source_commit(2, 12, "child", "root", 'c', false),
            ),
        )
    })
    .unwrap();
    let replayed_id = event_id('a');
    let orphan_id = event_id('b');
    let original_payload: (String, i64, String, String, String, i64, i64) = {
        let connection = fixture.ledger.connection().unwrap();
        clone_event_as(&connection, 1, 1, &replayed_id, &orphan_id);
        connection
            .execute(
                "INSERT INTO codex_usage_event_occurrences(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,event_id,created_at_ms)
                 VALUES ('codex',1,1,1,50,60,?1,20)",
                [&orphan_id],
            )
            .unwrap();
        connection
            .query_row(
                "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,model,
                        input_tokens,total_tokens FROM usage_events
                 WHERE source='codex' AND source_epoch=1 AND event_id=?1",
                [&replayed_id],
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
            .unwrap()
    };
    {
        let connection = fixture.ledger.connection().unwrap();
        insert_fact(
            &connection,
            1,
            &replayed_id,
            "replay-response",
            "compaction",
        );
        connection
            .execute(
                "INSERT INTO codex_compaction_markers(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                    reasoning_effort,response_id,resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,30,31,'child','root',30,'model',NULL,
                         'replay-response',?1,NULL)",
                [&replayed_id],
            )
            .unwrap();
    }
    let mut replay = source_commit(1, 11, "child", "root", 'a', false);
    let expected_state;
    {
        let connection = fixture.ledger.connection().unwrap();
        replay.expected_checkpoint = read_usage_checkpoint(&connection, 1).unwrap().unwrap();
        expected_state = read_usage_source_state(&connection, 1, 1).unwrap().unwrap();
        replay.expected_state = Some(expected_state.clone());
    }
    replay.reconciliation_request =
        crate::codex::ingestion::usage_processor::ReconciliationRequest::new(
            vec![crate::codex::ingestion::usage_processor::ResponseKey {
                owning_thread_id: "child".to_owned(),
                response_id: "replay-response".to_owned(),
            }],
            Vec::new(),
        );
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM codex_usage_event_occurrences
             WHERE source='codex' AND ledger_epoch=1 AND source_file_id=1 AND event_id=?1",
            [&replayed_id],
        )
        .unwrap();
    let marker_only_context =
        response_reconciliation_context(&fixture, &replay, "replay-response").unwrap();
    let replay_key = crate::codex::ingestion::usage_processor::ResponseKey {
        owning_thread_id: "child".to_owned(),
        response_id: "replay-response".to_owned(),
    };
    assert_eq!(
        marker_only_context.context.bindings[&replay_key]
            .proposal
            .event_id,
        replayed_id
    );
    let marker_only_binding = &marker_only_context.context.bindings[&replay_key].proposal;
    assert_eq!(marker_only_binding.kind, EventKind::Normal);
    assert_eq!(marker_only_binding.occurred_at_ms, 5);
    assert_eq!(marker_only_binding.thread_id, "child");
    assert_eq!(marker_only_binding.root_session_id, "root");
    assert_eq!(marker_only_binding.model, "model");
    assert_eq!(marker_only_binding.reasoning_effort, None);
    assert_eq!(marker_only_binding.usage, vector());
    assert!(marker_only_context.response_occurrences[&replay_key].is_empty());
    assert!(
        marker_only_context
            .context
            .markers
            .iter()
            .any(|marker| { marker.resolved_event_id.as_deref() == Some(replayed_id.as_str()) })
    );
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO codex_usage_event_occurrences(
                source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,event_id,created_at_ms)
             VALUES ('codex',1,1,1,0,20,?1,20)",
            [&replayed_id],
        )
        .unwrap();
    replay.local_replay = true;
    replay.fixed_view_exhausted = true;
    replay.tail_status = UsageTailStatus::None;
    replay.fixed_observed_raw_size = 100;
    replay.last_complete_offset = 100;
    replay.source_bytes_consumed = 100;
    replay.complete_line_count = 3;
    replay.updated_state = state("child", "root", 11, 100, false);
    replay.updated_state.observed_raw_size = 100;
    replay.updated_state.raw_tail_status = UsageTailStatus::None;
    replay.updated_state.raw_tail_start_offset = None;
    replay.updated_state.updated_at_ms = 200;
    replay.committed_at_ms = 200;
    replay.next_guard_hash = Some(vec![10; 32]);
    replay.patch.occurrences = vec![UsageOccurrenceWrite {
        source_file_id: 1,
        file_generation: 1,
        source_start_offset: 0,
        source_end_offset: 20,
        event_id: replayed_id.clone(),
    }];
    replay.patch.facts = vec![UsageEventFactWrite {
        event_id: replayed_id.clone(),
        owning_thread_id: "child".to_owned(),
        response_id: Some("replay-response".to_owned()),
        evidence_kind: EvidenceKind::Explicit,
        operation: CodexOperation::Compaction,
    }];
    replay.patch.marker_upserts = vec![UsageCompactionMarkerWrite {
        source_file_id: 1,
        file_generation: 1,
        source_start_offset: 30,
        source_end_offset: 31,
        owning_thread_id: "child".to_owned(),
        root_session_id: "root".to_owned(),
        occurred_at_ms: Some(30),
        model: Some("model".to_owned()),
        reasoning_effort: None,
        response_id: Some("replay-response".to_owned()),
        resolved_event_id: Some(replayed_id.clone()),
        unknown_reason: None,
    }];
    let counts = patch_counts(&replay.patch).unwrap();
    replay.canonical_event_count = counts.0;
    replay.occurrence_count = counts.1;
    replay.evidence_write_count = counts.2;
    replay.write_unit_count = counts.3;
    let replay_batch = batch("child", "root", replay.clone());
    {
        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        prepare_local_replay(&transaction, &replay_batch, &replay).unwrap();
        assert!(local_replay_orphan_ids(&transaction, 1).unwrap().is_empty());
        let held: Vec<String> = {
            let mut statement = transaction
                .prepare(
                    "SELECT event_id FROM codex_usage_event_holds
                     WHERE ledger_epoch=1 AND source_file_id=1 AND hold_reason='replay'
                     ORDER BY event_id",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(held, vec![replayed_id.clone(), orphan_id.clone()]);
        transaction.commit().unwrap();
    }

    let restarted = Ledger::open(LedgerOptions::new(fixture.ledger.database_path())).unwrap();
    let (canonical, occurrences, markers, holds): (i64, i64, i64, i64) = restarted
        .connection()
        .unwrap()
        .query_row(
            "SELECT
                (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=1 AND event_id IN (?1,?2)),
                (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=1 AND event_id IN (?1,?2)),
                (SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=1 AND resolved_event_id=?1),
                (SELECT count(*) FROM codex_usage_event_holds WHERE ledger_epoch=1 AND event_id IN (?1,?2))",
            params![replayed_id, orphan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((canonical, occurrences, markers, holds), (2, 0, 0, 2));

    // Fault-inject the state CAS snapshot that a resumed replay uses after
    // preparation; holds and deleted old references remain the durable state.
    with_codex(&restarted, |storage| {
        let mut tx = storage.begin_write_txn().unwrap();
        tx.with_private_state(|connection| {
            write_source_state_row(connection, 1, 1, &expected_state)
                .map_err(CodexStorageError::from)
        })
        .unwrap();
        assert_eq!(
            crate::codex::storage::rebuild::delete_orphan_events(
                &mut tx,
                1,
                UsageWriteTarget::Active,
            )
            .unwrap(),
            0
        );
        tx.commit().unwrap();
    });

    let mut held_dependency = source_commit(2, 12, "child", "root", 'c', false);
    {
        let connection = restarted.connection().unwrap();
        held_dependency.expected_checkpoint =
            read_usage_checkpoint(&connection, 2).unwrap().unwrap();
        held_dependency.expected_state = read_usage_source_state(&connection, 1, 2).unwrap();
        held_dependency.reconciliation_request = replay.reconciliation_request.clone();
    }
    let held_context =
        response_reconciliation_context(&fixture, &held_dependency, "replay-response").unwrap();
    assert!(held_context.response_occurrences[&replay_key].is_empty());
    let held_binding = &held_context.context.bindings[&replay_key].proposal;
    assert_eq!(held_binding.event_id, replayed_id);
    assert_eq!(held_binding.kind, EventKind::Normal);
    assert_eq!(held_binding.occurred_at_ms, 5);
    assert_eq!(held_binding.thread_id, "child");
    assert_eq!(held_binding.root_session_id, "root");
    assert_eq!(held_binding.model, "model");
    assert_eq!(held_binding.reasoning_effort, None);
    assert_eq!(held_binding.usage, vector());
    held_dependency.reconciliation_expected_fingerprint =
        held_context.context.expected_fingerprint.clone();
    let held_batch = batch("child", "root", held_dependency.clone());
    {
        let connection = restarted.connection().unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET current_path='/tmp/usage-1-rebound.jsonl'
                 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert!(
            validate_reconciliation_context(&connection, &held_batch, &held_dependency).is_err()
        );
        connection
            .execute(
                "UPDATE codex_source_files SET current_path='/tmp/usage-1.jsonl'
                 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET file_generation=2 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert!(
            response_reconciliation_context(&fixture, &held_dependency, "replay-response").is_err()
        );
        connection
            .execute(
                "UPDATE codex_source_files SET file_generation=1 WHERE source_file_id=1",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id='other-root' WHERE source_file_id=1",
                [],
            )
            .unwrap();
        assert!(
            response_reconciliation_context(&fixture, &held_dependency, "replay-response").is_err()
        );
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id='child' WHERE source_file_id=1",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_usage_event_holds(
                    source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
                 VALUES ('codex',1,2,1,?1,'carry')",
                [&replayed_id],
            )
            .unwrap();
        assert!(
            validate_reconciliation_context(&connection, &held_batch, &held_dependency).is_err()
        );
        connection
            .execute(
                "DELETE FROM codex_usage_event_holds
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=2
                   AND file_generation=1 AND event_id=?1",
                [&replayed_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_usage_event_holds(
                    source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
                 VALUES ('codex',1,2,1,?1,'carry')",
                [&replayed_id],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM codex_usage_event_holds
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=1
                   AND file_generation=1 AND event_id=?1",
                [&replayed_id],
            )
            .unwrap();
        assert!(
            validate_reconciliation_context(&connection, &held_batch, &held_dependency).is_err()
        );
        connection
            .execute(
                "INSERT INTO codex_usage_event_holds(
                    source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
                 VALUES ('codex',1,1,1,?1,'replay')",
                [&replayed_id],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM codex_usage_event_holds
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=2
                   AND file_generation=1 AND event_id=?1",
                [&replayed_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_usage_event_holds SET hold_reason='carry'
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=1
                   AND file_generation=1 AND event_id=?1",
                [&replayed_id],
            )
            .unwrap();
        assert!(
            validate_reconciliation_context(&connection, &held_batch, &held_dependency).is_err()
        );
        connection
            .execute(
                "UPDATE codex_usage_event_holds SET hold_reason='replay'
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=1
                   AND file_generation=1 AND event_id=?1",
                [&replayed_id],
            )
            .unwrap();
    }

    let mut fact_only = held_dependency.clone();
    fact_only.reconciliation_request =
        crate::codex::ingestion::usage_processor::ReconciliationRequest::new(
            vec![crate::codex::ingestion::usage_processor::ResponseKey {
                owning_thread_id: "child".to_owned(),
                response_id: "orphan-response".to_owned(),
            }],
            Vec::new(),
        );
    {
        let connection = restarted.connection().unwrap();
        connection
            .execute(
                "DELETE FROM codex_usage_event_holds
                 WHERE source='codex' AND ledger_epoch=1 AND source_file_id=1
                   AND file_generation=1 AND event_id=?1",
                [&orphan_id],
            )
            .unwrap();
        insert_fact(&connection, 1, &orphan_id, "orphan-response", "response");
    }
    assert!(response_reconciliation_context(&fixture, &fact_only, "orphan-response").is_err());
    {
        let connection = restarted.connection().unwrap();
        connection
            .execute(
                "DELETE FROM codex_usage_event_facts
                 WHERE source='codex' AND ledger_epoch=1 AND event_id=?1",
                [&orphan_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_usage_event_holds(
                    source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
                 VALUES ('codex',1,1,1,?1,'replay')",
                [&orphan_id],
            )
            .unwrap();
    }

    with_codex(&restarted, |storage| {
        test_commit_group(storage, batch("child", "root", replay))
    })
    .unwrap();
    let connection = restarted.connection().unwrap();
    let remaining_replayed: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=1 AND event_id=?1),
                (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=1 AND event_id=?1),
                (SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=1 AND resolved_event_id=?1),
                (SELECT count(*) FROM codex_usage_event_holds WHERE ledger_epoch=1 AND event_id=?1)",
            [&replayed_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(remaining_replayed, (1, 1, 1, 0));
    let remaining_orphan: i64 = connection
        .query_row(
            "SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=1 AND event_id=?1",
            [&orphan_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_orphan, 0);
    let replayed_payload: (String, i64, String, String, String, i64, i64) = connection
        .query_row(
            "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,model,
                    input_tokens,total_tokens FROM usage_events
             WHERE source='codex' AND source_epoch=1 AND event_id=?1",
            [&replayed_id],
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
    assert_eq!(replayed_payload, original_payload);
}

#[test]
fn compaction_closed_turn_late_rewrite() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture.add_source(2, Some("child"), 12);
    for (source_id, device, event) in [(1, 11, 'a'), (2, 12, 'c')] {
        with_codex(&fixture.ledger, |storage| {
            test_commit_group(
                storage,
                batch(
                    "child",
                    "root",
                    source_commit(source_id, device, "child", "root", event, false),
                ),
            )
        })
        .unwrap();
        set_active_proof(&fixture, source_id, 100);
    }

    let old_compensation_id = event_id('b');
    let new_compensation_id = event_id('d');
    let late_response_id = event_id('e');
    let original_turn = {
        let connection = fixture.ledger.connection().unwrap();
        let mut turn = source_commit(1, 11, "child", "root", 'a', true)
            .patch
            .turn_upserts
            .remove(0);
        turn.ended_at_ms = Some(80);
        turn.end_offset = Some(80);
        turn.status = UsageTurnStatus::Completed;
        turn.state_through_offset = 80;
        turn.updated_at_ms = 50;
        write_turn(&connection, 1, 1, 1, "child", &turn).unwrap();
        clone_event_as(&connection, 1, 1, &event_id('a'), &old_compensation_id);
        connection
            .execute(
                "UPDATE usage_events SET event_kind='turn_compensation',turn_key='turn',
                    occurred_at_ms=80
                 WHERE source='codex' AND source_epoch=1 AND event_id=?1",
                [&old_compensation_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_usage_event_occurrences(
                    source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,event_id,created_at_ms)
                 VALUES ('codex',1,1,1,70,80,?1,50),
                        ('codex',1,2,1,70,80,?1,50)",
                [&old_compensation_id],
            )
            .unwrap();
        let mut sibling_turn = turn.clone();
        sibling_turn.source_file_id = 2;
        write_turn(&connection, 1, 2, 1, "child", &sibling_turn).unwrap();
        turn
    };

    let mut late = source_commit(2, 12, "child", "root", 'e', false);
    {
        let connection = fixture.ledger.connection().unwrap();
        late.expected_checkpoint = read_usage_checkpoint(&connection, 2).unwrap().unwrap();
        late.expected_state = read_usage_source_state(&connection, 1, 2).unwrap();
        late.expected_file_generation = 1;
        late.batch_start_offset = 100;
        late.fixed_observed_raw_size = 140;
        late.last_complete_offset = 120;
        late.source_bytes_consumed = 20;
        late.complete_line_count = 1;
        late.next_guard_hash = Some(vec![10; 32]);
        late.updated_state = state("child", "root", 12, 120, false);
        late.updated_state.observed_raw_size = 140;
        late.updated_state.updated_at_ms = 100;
        connection
            .execute(
                "UPDATE codex_source_files SET observed_size=140 WHERE source_file_id=2",
                [],
            )
            .unwrap();
    }
    let mut late_response = late.patch.events[0].clone();
    late_response.event_id = late_response_id.clone();
    late_response.occurred_at_ms = 110;
    late_response.turn_key = Some("turn".to_owned());
    let mut replacement_compensation = late_response.clone();
    replacement_compensation.event_id = new_compensation_id.clone();
    replacement_compensation.kind = EventKind::TurnCompensation;
    replacement_compensation.occurred_at_ms = 80;
    late.patch.events = vec![late_response, replacement_compensation];
    late.patch.occurrences = vec![
        UsageOccurrenceWrite {
            source_file_id: 1,
            file_generation: 1,
            source_start_offset: 70,
            source_end_offset: 80,
            event_id: new_compensation_id.clone(),
        },
        UsageOccurrenceWrite {
            source_file_id: 2,
            file_generation: 1,
            source_start_offset: 70,
            source_end_offset: 80,
            event_id: new_compensation_id.clone(),
        },
        UsageOccurrenceWrite {
            source_file_id: 2,
            file_generation: 1,
            source_start_offset: 100,
            source_end_offset: 110,
            event_id: late_response_id.clone(),
        },
    ];
    let mut rewritten_turn = original_turn.clone();
    rewritten_turn.accounted_candidate_count += 1;
    late.patch.delete_event_ids = vec![old_compensation_id.clone()];
    late.patch.turn_rewrites = vec![UsageTurnRewriteWrite {
        expected: original_turn,
        replacement: rewritten_turn,
    }];
    late.reconciliation_request =
        crate::codex::ingestion::usage_processor::ReconciliationRequest::new(
            Vec::new(),
            vec![("child".to_owned(), Some("turn".to_owned()))],
        );

    let basic_proof = UsageReconciliationBasicProof {
        device_id: 12,
        inode: 12,
        observed_raw_size: 140,
        expected_checkpoint: Some(late.expected_checkpoint.clone()),
        expected_state: late.expected_state.clone(),
    };
    let context = with_codex(&fixture.ledger, |storage| {
        storage.load_usage_reconciliation_context(
            1,
            crate::codex::ingestion::usage_processor::UsageContext {
                source_file_id: 2,
                file_generation: 1,
                owning_thread_id: "child".to_owned(),
                root_session_id: "root".to_owned(),
            },
            late.reconciliation_request.clone(),
            basic_proof,
        )
    })
    .unwrap();
    assert_eq!(context.context.affected_turns.len(), 2);
    for source_file_id in [1, 2] {
        let turn_key = crate::codex::ingestion::usage_processor::PersistedTurnKey {
            source_file_id,
            file_generation: 1,
            turn_key: "turn".to_owned(),
        };
        let affected = context
            .context
            .affected_turns
            .get(&turn_key)
            .expect("complete owner/Turn closure includes every source snapshot");
        assert_eq!(affected.compensation_events.len(), 1);
        assert_eq!(
            affected.compensation_events[0].event_id,
            old_compensation_id
        );
        assert_eq!(affected.compensation_occurrences.len(), 2);
        assert!(affected.compensation_occurrences.iter().any(|occurrence| {
            occurrence.source_file_id == 1
                && occurrence.file_generation == 1
                && occurrence.source_start_offset == 70
                && occurrence.source_end_offset == 80
        }));
        assert!(affected.compensation_occurrences.iter().any(|occurrence| {
            occurrence.source_file_id == 2
                && occurrence.file_generation == 1
                && occurrence.source_start_offset == 70
                && occurrence.source_end_offset == 80
        }));
    }
    late.reconciliation_expected_fingerprint = context.context.expected_fingerprint;
    let late_batch = batch("child", "root", late.clone());
    validate_reconciliation_context(&fixture.ledger.connection().unwrap(), &late_batch, &late)
        .unwrap();

    let mut response_before_chunk = late.clone();
    response_before_chunk.patch.occurrences[2].source_start_offset = 90;
    response_before_chunk.patch.occurrences[2].source_end_offset = 100;
    assert!(
        validate_reconciliation_context(
            &fixture.ledger.connection().unwrap(),
            &batch("child", "root", response_before_chunk.clone()),
            &response_before_chunk,
        )
        .is_err()
    );

    let mut unproved_occurrence = late.clone();
    unproved_occurrence
        .patch
        .occurrences
        .push(UsageOccurrenceWrite {
            source_file_id: 3,
            file_generation: 1,
            source_start_offset: 10,
            source_end_offset: 20,
            event_id: event_id('f'),
        });
    assert!(
        validate_reconciliation_context(
            &fixture.ledger.connection().unwrap(),
            &batch("child", "root", unproved_occurrence.clone()),
            &unproved_occurrence,
        )
        .is_err()
    );

    let mut without_turn_rewrite = late.clone();
    without_turn_rewrite.patch.turn_rewrites.clear();
    assert!(
        validate_reconciliation_context(
            &fixture.ledger.connection().unwrap(),
            &batch("child", "root", without_turn_rewrite.clone()),
            &without_turn_rewrite,
        )
        .is_err()
    );

    let mut classification_only = late;
    classification_only.patch.events.clear();
    classification_only.patch.occurrences.clear();
    classification_only.patch.delete_event_ids.clear();
    classification_only.patch.turn_rewrites.clear();
    classification_only.patch.facts = vec![UsageEventFactWrite {
        event_id: event_id('a'),
        owning_thread_id: "child".to_owned(),
        response_id: Some("response-a".to_owned()),
        evidence_kind: EvidenceKind::Explicit,
        operation: CodexOperation::Compaction,
    }];
    classification_only.reconciliation_request =
        crate::codex::ingestion::usage_processor::ReconciliationRequest::default();
    let counts = patch_counts(&classification_only.patch).unwrap();
    classification_only.canonical_event_count = counts.0;
    classification_only.occurrence_count = counts.1;
    classification_only.evidence_write_count = counts.2;
    classification_only.write_unit_count = counts.3;
    validate_source_payload(
        &batch("child", "root", classification_only.clone()),
        &classification_only,
    )
    .unwrap();
    let basic_proof = UsageReconciliationBasicProof {
        device_id: 12,
        inode: 12,
        observed_raw_size: 140,
        expected_checkpoint: Some(classification_only.expected_checkpoint.clone()),
        expected_state: classification_only.expected_state.clone(),
    };
    let context = with_codex(&fixture.ledger, |storage| {
        storage.load_usage_reconciliation_context(
            1,
            crate::codex::ingestion::usage_processor::UsageContext {
                source_file_id: 2,
                file_generation: 1,
                owning_thread_id: "child".to_owned(),
                root_session_id: "root".to_owned(),
            },
            classification_only.reconciliation_request.clone(),
            basic_proof,
        )
    })
    .unwrap();
    classification_only.reconciliation_expected_fingerprint = context.context.expected_fingerprint;
    validate_reconciliation_context(
        &fixture.ledger.connection().unwrap(),
        &batch("child", "root", classification_only.clone()),
        &classification_only,
    )
    .unwrap();
}

#[test]
fn compaction_metadata_reconcile() {
    for (root_changed, binding_changed) in [(true, false), (false, true), (true, true)] {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        with_codex(&fixture.ledger, |storage| {
            test_commit_group(
                storage,
                batch(
                    "child",
                    "root",
                    source_commit(1, 11, "child", "root", 'a', false),
                ),
            )
        })
        .unwrap();
        let id = event_id('a');
        {
            let connection = fixture.ledger.connection().unwrap();
            insert_fact(&connection, 1, &id, "response-a", "compaction");
            connection
                .execute(
                    "INSERT INTO codex_compaction_markers(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                        reasoning_effort,response_id,resolved_event_id,unknown_reason)
                     VALUES ('codex',1,1,1,30,31,'child','root',30,'model',NULL,'response-a',?1,NULL),
                            ('codex',1,1,1,40,41,'child','root',NULL,NULL,NULL,NULL,NULL,'time_missing')",
                    [&id],
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
            clone_event(&connection, 1, 2, &id);
            insert_fact(&connection, 2, &id, "response-a", "compaction");
            connection
                .execute(
                    "INSERT INTO codex_usage_event_occurrences(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id,created_at_ms)
                     VALUES ('codex',2,1,1,0,20,?1,10)",
                    [&id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_compaction_markers(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                        reasoning_effort,response_id,resolved_event_id,unknown_reason)
                     VALUES ('codex',2,1,1,30,31,'child','root',30,'model',NULL,'response-a',?1,NULL),
                            ('codex',2,1,1,40,41,'child','root',NULL,NULL,NULL,NULL,NULL,'time_missing')",
                    [&id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_usage_reconciliation_windows(
                        source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,owning_thread_id,turn_key,state_json)
                     VALUES ('codex',2,1,1,50,51,'child',NULL,?1)",
                    [empty_window_json()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_usage_event_holds(
                        source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
                     VALUES ('codex',2,1,1,?1,'carry')",
                    [&id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_build_sources SET carry_from_epoch=1,carry_phase='facts',
                        carry_after_fact_event_id=?1,carry_after_marker_start_offset=30,
                        carry_after_window_start_offset=50
                     WHERE build_epoch=2 AND source_file_id=1",
                    [&id],
                )
                .unwrap();
        }
        let root_next = if root_changed { "other-root" } else { "root" };
        with_codex(&fixture.ledger, |storage| {
            let mut tx = storage.begin_write_txn().unwrap();
            tx.with_private_state(|connection| {
                if root_changed {
                    connection
                        .execute(
                            "UPDATE threads SET root_session_id='other-root' WHERE thread_id='child'",
                            [],
                        )
                        .map_err(CodexStorageError::from)?;
                }
                if binding_changed {
                    connection
                        .execute(
                            "UPDATE codex_source_files SET thread_id='other-root'
                             WHERE source_file_id=1",
                            [],
                        )
                        .map_err(CodexStorageError::from)?;
                }
                Ok::<_, CodexStorageError>(())
            })
            .unwrap();
            reconcile_usage_metadata_change(
                &mut tx,
                "child",
                Some("root"),
                Some(root_next),
                if binding_changed { &[1] } else { &[] },
            )
            .unwrap();
            tx.commit().unwrap();
        });

        let connection = fixture.ledger.connection().unwrap();
        let event_root: String = connection
            .query_row(
                "SELECT root_session_id FROM usage_events
                 WHERE source='codex' AND source_epoch=1 AND event_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        let owner: String = connection
            .query_row(
                "SELECT owning_thread_id FROM codex_usage_event_facts
                 WHERE ledger_epoch=1 AND event_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        let marker_roots = connection
            .prepare(
                "SELECT root_session_id FROM codex_compaction_markers
                 WHERE ledger_epoch=1 AND source_file_id=1 ORDER BY source_start_offset",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(event_root, root_next);
        assert_eq!(owner, "child");
        assert_eq!(
            marker_roots,
            vec![root_next.to_owned(), root_next.to_owned()]
        );

        let reset: (String, String, Option<String>, Option<i64>, Option<i64>, i64, i64, i64, i64) =
            connection
                .query_row(
                    "SELECT completion_status,carry_phase,carry_after_fact_event_id,
                        carry_after_marker_start_offset,carry_after_window_start_offset,
                        (SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=1),
                        (SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=2 AND source_file_id=1),
                        (SELECT count(*) FROM codex_usage_reconciliation_windows WHERE ledger_epoch=2 AND source_file_id=1),
                        (SELECT count(*) FROM codex_usage_event_holds WHERE ledger_epoch=2 AND source_file_id=1)
                     FROM codex_usage_build_sources WHERE build_epoch=2 AND source_file_id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
                )
                .unwrap();
        assert_eq!(
            reset,
            (
                "pending".to_owned(),
                "none".to_owned(),
                None,
                None,
                None,
                0,
                0,
                0,
                0
            )
        );
    }
}

#[test]
fn compaction_lifecycle() {
    let (fixture, marker_event, response_event) = prepare_marker_window_carry();
    for step in 0..7 {
        with_codex(&fixture.ledger, |storage| {
            storage.resume_carry(1, 100 + step).unwrap()
        });
    }
    let connection = fixture.ledger.connection().unwrap();
    let rows: (i64, i64, String, String) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=1 AND event_id=?1),
                (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=2 AND event_id=?1),
                (SELECT operation FROM codex_usage_event_facts WHERE ledger_epoch=1 AND event_id=?1),
                (SELECT operation FROM codex_usage_event_facts WHERE ledger_epoch=2 AND event_id=?1)",
            [&response_event],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(rows, (1, 1, "response".to_owned(), "response".to_owned()));
    let marker_operations: (String, String) = connection
        .query_row(
            "SELECT
                (SELECT operation FROM codex_usage_event_facts WHERE ledger_epoch=1 AND event_id=?1),
                (SELECT operation FROM codex_usage_event_facts WHERE ledger_epoch=2 AND event_id=?1)",
            [&marker_event],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        marker_operations,
        ("compaction".to_owned(), "compaction".to_owned())
    );
    let carried_marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM codex_compaction_markers
             WHERE ledger_epoch=2 AND resolved_event_id=?1",
            [&marker_event],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(carried_marker, 1);
    let foreign_key_errors: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(foreign_key_errors, 0);
    assert_partial_build_quarantine_lifecycle();
}

fn assert_partial_build_quarantine_lifecycle() {
    for preserve_active in [false, true] {
        let fixture = Fixture::new();
        let (active_epoch, build_epoch, build_source_id, source_ids) = if preserve_active {
            fixture.add_source(1, Some("child"), 11);
            with_codex(&fixture.ledger, |storage| {
                test_commit_group(
                    storage,
                    batch(
                        "child",
                        "root",
                        source_commit(1, 11, "child", "root", 'a', false),
                    ),
                )
            })
            .unwrap();
            set_active_proof(&fixture, 1, 20);
            fixture.add_source(2, Some("child"), 12);
            (1, 2, 2, vec![1, 2])
        } else {
            fixture
                .ledger
                .connection()
                .unwrap()
                .execute(
                    "UPDATE source_usage_epochs SET active_epoch=0 WHERE source='codex'",
                    [],
                )
                .unwrap();
            fixture.add_source(1, Some("child"), 11);
            (0, 1, 1, vec![1])
        };

        let snapshot = with_codex(&fixture.ledger, |storage| {
            storage.begin_rebuild(
                crate::codex::normalization::USAGE_PARSER_VERSION,
                &source_ids,
                20,
            )
        })
        .unwrap();
        assert_eq!(snapshot.active_epoch, active_epoch);
        assert_eq!(snapshot.build_epoch, build_epoch);

        let mut partial = source_commit(
            build_source_id,
            if build_source_id == 1 { 11 } else { 12 },
            "child",
            "root",
            'b',
            false,
        );
        partial.expected_checkpoint.processing_status = CheckpointProcessingStatus::RebuildRequired;
        let mut partial_batch = batch("child", "root", partial);
        partial_batch.ledger_epoch = build_epoch;
        with_codex(&fixture.ledger, |storage| {
            test_commit_group(storage, partial_batch)
        })
        .unwrap();
        let persisted_partial: i64 = fixture
            .ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE source='codex' AND source_epoch=?1 AND event_id=?2",
                params![build_epoch, event_id('b')],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_partial, 1);

        let diagnostic = UsageQuarantineDiagnostic {
            source_file_id: build_source_id,
            file_generation: 1,
            owning_thread_id: "child".to_owned(),
            anomaly: UsageAnomalyWrite {
                anomaly_id: event_id('c'),
                detected_at_ms: 30,
                occurred_at_ms: Some(20),
                kind: UsageAnomalyKind::LegacyCoverageAmbiguous,
                severity_error: true,
                source_start_offset: Some(20),
                turn_key: Some("turn".to_owned()),
            },
        };
        let quarantined = with_codex(&fixture.ledger, |storage| {
            storage.quarantine_thread(
                "child",
                "USAGE_LEGACY_COVERAGE_AMBIGUOUS",
                Some(diagnostic),
                30,
            )
        })
        .unwrap();
        assert_eq!(quarantined, source_ids.len());

        let connection = fixture.ledger.connection().unwrap();
        let epoch_state: (i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(epoch_state, (active_epoch, Some(build_epoch)));

        let build_rows: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM codex_usage_build_sources
                     WHERE build_epoch=?1 AND completion_status='quarantined'),
                    (SELECT count(*) FROM codex_usage_event_occurrences
                     WHERE ledger_epoch=?1),
                    (SELECT count(*) FROM usage_events
                     WHERE source='codex' AND source_epoch=?1 AND root_session_id='root'),
                    (SELECT count(*) FROM codex_ingest_anomalies
                     WHERE ledger_epoch=?1 AND anomaly_type='LEGACY_COVERAGE_AMBIGUOUS')",
                [build_epoch],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(build_rows, (source_ids.len() as i64, 0, 0, 1));

        let active_events: i64 = connection
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE source='codex' AND source_epoch=?1 AND event_id=?2",
                params![active_epoch, event_id('a')],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_events, if preserve_active { 1 } else { 0 });
    }
}
