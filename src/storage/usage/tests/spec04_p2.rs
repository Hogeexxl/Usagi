use super::*;

fn fixture_path(name: &str) -> String {
    std::env::temp_dir()
        .join("usagi-storage-usage")
        .join(name.trim_start_matches('/'))
        .to_string_lossy()
        .into_owned()
}

fn set_active_source_proof(fixture: &Fixture, source_id: i64, active_offset: i64, guard_byte: u8) {
    let connection = fixture.ledger.connection().unwrap();
    connection
        .execute(
            "UPDATE source_files SET observed_size=?2,observed_mtime_ns=1
             WHERE source_file_id=?1",
            params![source_id, active_offset],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE source_checkpoints SET committed_offset=?2,guard_hash=?3,
                processing_status='ready',last_error_code=NULL
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            params![source_id, active_offset, vec![guard_byte; 32]],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE usage_source_states SET resolved_through_offset=?2,
                observed_raw_size=?2,raw_tail_status='none',raw_tail_start_offset=NULL,
                previous_total_offset=?2,updated_at_ms=20
             WHERE ledger_epoch=1 AND source_file_id=?1",
            params![source_id, active_offset],
        )
        .unwrap();
}

fn add_bulk_active_carry_facts(
    fixture: &Fixture,
    source_id: i64,
    occurrence_count: i64,
    turn_count: i64,
    anomaly_count: i64,
    active_offset: i64,
) {
    assert!(occurrence_count >= 1);
    assert!(turn_count >= 1);
    assert!(anomaly_count >= 1);
    let mut connection = fixture.ledger.connection().unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let event_id = "a".repeat(64);

    for index in 1..occurrence_count {
        let start = index * 10;
        transaction
            .execute(
                "INSERT INTO usage_event_occurrences(
                    ledger_epoch,source_file_id,file_generation,source_start_offset,
                    source_end_offset,event_id,created_at_ms
                 ) VALUES (1,?1,1,?2,?3,?4,10)",
                params![source_id, start, start + 1, event_id],
            )
            .unwrap();
    }

    let template_turn = source_commit(source_id, 11, "child", "root", 'a', true)
        .turns
        .into_iter()
        .next()
        .unwrap();
    for index in 1..turn_count {
        let start = index * 10 + 1;
        let mut turn = template_turn.clone();
        turn.turn_key = format!("bulk-turn-{index:08}");
        turn.raw_turn_id = Some(format!("bulk-raw-{index:08}"));
        turn.started_at_ms = Some(index);
        turn.ended_at_ms = Some(index + 1);
        turn.start_offset = start;
        turn.end_offset = Some(start + 1);
        turn.status = UsageTurnStatus::Completed;
        turn.quality_status = "complete";
        turn.state_through_offset = active_offset;
        turn.updated_at_ms = 20;
        write_turn(&transaction, 1, source_id, 1, "child", &turn).unwrap();
    }

    for index in 1..anomaly_count {
        let anomaly = UsageAnomalyWrite {
            anomaly_id: format!("{:064x}", index + 1),
            detected_at_ms: index + 10,
            occurred_at_ms: Some(index + 10),
            kind: UsageAnomalyKind::TurnReplaced,
            severity_error: false,
            source_start_offset: Some(index * 10 + 2),
        };
        write_anomaly(&transaction, 1, "child", source_id, 1, &anomaly).unwrap();
    }
    transaction.commit().unwrap();
}

fn prepare_carry_fixture(
    occurrence_count: i64,
    turn_count: i64,
    anomaly_count: i64,
) -> (Fixture, i64) {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture.add_source(2, Some("child"), 22);
    fixture
        .ledger
        .commit_usage(&batch(
            "child",
            "root",
            source_commit(1, 11, "child", "root", 'a', true),
        ))
        .unwrap();
    fixture
        .ledger
        .commit_usage(&batch(
            "child",
            "root",
            source_commit(2, 22, "child", "root", 'c', false),
        ))
        .unwrap();

    let active_offset = (occurrence_count.max(turn_count).max(anomaly_count) * 10 + 100).max(20);
    set_active_source_proof(&fixture, 1, active_offset, 9);
    set_active_source_proof(&fixture, 2, active_offset, 7);
    if occurrence_count > 1 || turn_count > 1 || anomaly_count > 1 {
        add_bulk_active_carry_facts(
            &fixture,
            1,
            occurrence_count,
            turn_count,
            anomaly_count,
            active_offset,
        );
    }

    {
        let mut connection = fixture.ledger.connection().unwrap();
        crate::usage::rebuild::RebuildLedger::new(&mut connection)
            .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 30)
            .unwrap();
    }

    // Give the unaffected member a completed build-only proof so every
    // replacement test can compare real progress rather than an empty row.
    let mut source2 = source_commit(2, 22, "child", "root", 'd', false);
    source2.expected_checkpoint.processing_status = CheckpointProcessingStatus::RebuildRequired;
    source2.fixed_observed_raw_size = active_offset;
    source2.last_complete_offset = active_offset;
    source2.source_bytes_consumed = active_offset;
    source2.fixed_view_exhausted = true;
    source2.tail_status = UsageTailStatus::None;
    source2.updated_state.resolved_through_offset = active_offset;
    source2.updated_state.observed_raw_size = active_offset;
    source2.updated_state.raw_tail_status = UsageTailStatus::None;
    source2.updated_state.raw_tail_start_offset = None;
    source2.updated_state.previous_total_offset = Some(active_offset);
    source2.next_guard_hash = Some(vec![7; 32]);
    fixture
        .ledger
        .commit_usage(&UsageCommitBatch {
            ledger_epoch: 2,
            usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
            thread_id: "child".to_owned(),
            root_session_id: "root".to_owned(),
            sources: vec![source2],
        })
        .unwrap();

    {
        let connection = fixture.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE source_files SET file_status='missing' WHERE source_file_id=1",
                [],
            )
            .unwrap();
    }
    fixture.ledger.begin_usage_carry(1, 40).unwrap();
    (fixture, active_offset)
}

fn carry_phase(fixture: &Fixture) -> UsageCarryPhase {
    fixture
        .ledger
        .load_usage_scan_state(&[1, 2], crate::usage::USAGE_PARSER_VERSION)
        .unwrap()
        .plans
        .into_iter()
        .find(|plan| plan.source_file_id == 1)
        .unwrap()
        .build
        .unwrap()
        .carry_phase
}

fn advance_carry_to(fixture: &Fixture, target: UsageCarryPhase) {
    for step in 0..32_i64 {
        if carry_phase(fixture) == target {
            return;
        }
        assert_ne!(carry_phase(fixture), UsageCarryPhase::None);
        fixture.ledger.resume_usage_carry(1, 100 + step).unwrap();
    }
    panic!("carry did not reach requested phase {target:?}");
}

fn present_carry_source(
    fixture: &Fixture,
    active_offset: i64,
    guard_matches: bool,
) -> crate::domain::SourceOutcome {
    present_carry_source_with_size(fixture, active_offset, active_offset, guard_matches)
}

fn present_carry_source_with_size(
    fixture: &Fixture,
    active_offset: i64,
    observed_size: i64,
    guard_matches: bool,
) -> crate::domain::SourceOutcome {
    let observations = vec![
        crate::domain::SourceObservation::new(
            fixture_path("usage-1.jsonl"),
            crate::domain::SourceArea::Sessions,
            11,
            11,
            observed_size,
            1,
            500,
        )
        .unwrap(),
        crate::domain::SourceObservation::new(
            fixture_path("usage-2.jsonl"),
            crate::domain::SourceArea::Sessions,
            22,
            22,
            active_offset,
            1,
            500,
        )
        .unwrap(),
    ];
    fixture
        .ledger
        .record_source_observations_with_usage_carry_proofs(
            crate::domain::SourceObservationBatch::new(
                observations,
                crate::domain::SourceRegionStatus::Complete,
                crate::domain::SourceRegionStatus::Complete,
            )
            .unwrap(),
            &[crate::storage::source::UsageCarryObservationProof {
                device_id: 11,
                inode: 11,
                active_committed_offset: active_offset,
                guard_matches,
            }],
        )
        .unwrap()
}

fn resume_carry_until_present_finalized(fixture: &Fixture) {
    for step in 0..32_i64 {
        match fixture.ledger.resume_usage_carry(1, 600 + step).unwrap() {
            CarryStepOutcome::FinalizedPresent => return,
            CarryStepOutcome::Progress => {}
            CarryStepOutcome::FinalizedMissing => {
                panic!("present source unexpectedly finalized as missing")
            }
        }
    }
    panic!("present carry did not finalize");
}

fn source2_build_snapshot(fixture: &Fixture) -> (String, i64, i64, String, i64, i64, i64) {
    let connection = fixture.ledger.connection().unwrap();
    connection
        .query_row(
            "SELECT b.completion_status,b.required_through_offset,b.completed_through_offset,
                    c.processing_status,c.committed_offset,
                    (SELECT count(*) FROM usage_event_occurrences
                     WHERE ledger_epoch=2 AND source_file_id=2),
                    (SELECT count(*) FROM usage_source_states
                     WHERE ledger_epoch=2 AND source_file_id=2)
             FROM usage_build_sources b
             JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                AND c.consumer_kind='usage'
             WHERE b.build_epoch=2 AND b.source_file_id=2",
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
        .unwrap()
}

#[test]
fn t_s04_048_planner_conflict_priority_matrix() {
    // parser bump must outrank otherwise eligible carry.
    let (fixture, _) = prepare_carry_fixture(1, 1, 1);
    let parser_bump = fixture
        .ledger
        .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION + 1)
        .unwrap();
    assert_eq!(
        parser_bump
            .plans
            .iter()
            .find(|plan| plan.source_file_id == 1)
            .unwrap()
            .action,
        UsagePlanAction::RebuildRequired
    );

    // Relationship uncertainty outranks a new-present read from zero.
    let relationship = Fixture::new();
    relationship.add_source(10, Some("unresolved"), 31);
    assert_eq!(
        relationship
            .ledger
            .load_usage_scan_state(&[10], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans[0]
            .action,
        UsagePlanAction::BlockedRelationship
    );

    // offset > raw is an error/rebuild branch even with a ready matching state.
    let offset_gt_raw = Fixture::new();
    offset_gt_raw.add_source(20, Some("child"), 41);
    offset_gt_raw
        .ledger
        .commit_usage(&batch(
            "child",
            "root",
            source_commit(20, 41, "child", "root", 'e', false),
        ))
        .unwrap();
    {
        let connection = offset_gt_raw.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE source_files SET observed_size=10 WHERE source_file_id=20",
                [],
            )
            .unwrap();
    }
    assert_eq!(
        offset_gt_raw
            .ledger
            .load_usage_scan_state(&[20], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans[0]
            .action,
        UsagePlanAction::RebuildRequired
    );

    // offset == raw + unverified is VerifyRawTail, never Skip.
    let verify_tail = Fixture::new();
    verify_tail.add_source(30, Some("child"), 51);
    verify_tail
        .ledger
        .commit_usage(&batch(
            "child",
            "root",
            source_commit(30, 51, "child", "root", 'f', false),
        ))
        .unwrap();
    {
        let connection = verify_tail.ledger.connection().unwrap();
        connection
            .execute(
                "UPDATE source_files SET observed_size=20 WHERE source_file_id=30",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE usage_source_states SET observed_raw_size=20,raw_tail_status='unverified',
                    raw_tail_start_offset=NULL WHERE ledger_epoch=1 AND source_file_id=30",
                [],
            )
            .unwrap();
    }
    assert_eq!(
        verify_tail
            .ledger
            .load_usage_scan_state(&[30], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans[0]
            .action,
        UsagePlanAction::VerifyRawTail
    );

    // offset < raw + ready consumes only the incremental suffix.
    let incremental = Fixture::new();
    incremental.add_source(40, Some("child"), 61);
    incremental
        .ledger
        .commit_usage(&batch(
            "child",
            "root",
            source_commit(40, 61, "child", "root", '1', false),
        ))
        .unwrap();
    assert_eq!(
        incremental
            .ledger
            .load_usage_scan_state(&[40], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans[0]
            .action,
        UsagePlanAction::ResumeOwningLive
    );
}

#[test]
fn t_s04_048_parser_six_rebuilds_parser_five_epoch() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute("UPDATE app_meta SET usage_parser_version=5 WHERE id=1", [])
        .unwrap();

    let plan = fixture
        .ledger
        .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
        .unwrap();
    assert_eq!(
        plan.plans[0].action,
        UsagePlanAction::RebuildRequired,
        "parser 6 must rebuild an active parser 5 epoch"
    );
    assert_eq!(crate::usage::canonical_algorithm_for(5), Some(4));
    assert_eq!(crate::usage::canonical_algorithm_for(6), Some(5));
}

#[test]
fn t_s04_049_carry_four_phase_present_resume_is_exact_and_complete_only_finishes() {
    for target in [
        UsageCarryPhase::Occurrences,
        UsageCarryPhase::Turns,
        UsageCarryPhase::Anomalies,
        UsageCarryPhase::Finalize,
    ] {
        let (fixture, active_offset) = prepare_carry_fixture(1, 1, 1);
        advance_carry_to(&fixture, target);

        let before = fixture.ledger.connection().unwrap();
        let active_occurrences: i64 = before
            .query_row(
                "SELECT count(*) FROM usage_event_occurrences
                 WHERE ledger_epoch=1 AND source_file_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(before);

        let observed = present_carry_source(&fixture, active_offset, true);
        let affected = observed
            .results
            .iter()
            .find(|result| result.source_file_id == 1)
            .unwrap();
        assert_eq!(
            affected.build_disposition,
            crate::domain::BuildDisposition::CarryResumedPresent,
            "phase {target:?}"
        );
        assert_eq!(
            carry_phase(&fixture),
            target,
            "phase cursor changed during observation"
        );

        resume_carry_until_present_finalized(&fixture);
        let plan = fixture
            .ledger
            .load_usage_scan_state(&[1, 2], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans
            .into_iter()
            .find(|plan| plan.source_file_id == 1)
            .unwrap();
        assert_eq!(
            plan.action,
            UsagePlanAction::CompleteOnly,
            "phase {target:?}"
        );

        let connection = fixture.ledger.connection().unwrap();
        let carried: (i64, i64, String, i64) = connection
            .query_row(
                "SELECT
                    count(*),
                    count(DISTINCT source_start_offset),
                    (SELECT c.processing_status FROM source_checkpoints c
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage'),
                    (SELECT c.committed_offset FROM source_checkpoints c
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage')
                 FROM usage_event_occurrences
                 WHERE ledger_epoch=2 AND source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            carried,
            (
                active_occurrences,
                active_occurrences,
                "ready".into(),
                active_offset
            ),
            "phase {target:?}"
        );
        drop(connection);

        fixture.ledger.complete_usage_build_source(1, 800).unwrap();
        let connection = fixture.ledger.connection().unwrap();
        let completion: String = connection
            .query_row(
                "SELECT completion_status FROM usage_build_sources
                 WHERE build_epoch=2 AND source_file_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(completion, "rebuilt", "phase {target:?}");
    }
}

#[test]
fn present_growth_then_missing_blocks_carry_finalize_with_unverified_tail() {
    let (fixture, active_offset) = prepare_carry_fixture(1, 1, 1);
    let observed =
        present_carry_source_with_size(&fixture, active_offset, active_offset + 10, true);
    assert_eq!(
        observed
            .results
            .iter()
            .find(|result| result.source_file_id == 1)
            .unwrap()
            .build_disposition,
        crate::domain::BuildDisposition::CarryResumedPresent
    );

    let source_two = crate::domain::SourceObservation::new(
        fixture_path("usage-2.jsonl"),
        crate::domain::SourceArea::Sessions,
        22,
        22,
        active_offset,
        1,
        501,
    )
    .unwrap();
    fixture
        .ledger
        .record_source_observations_with_usage_carry_proofs(
            crate::domain::SourceObservationBatch::new(
                vec![source_two],
                crate::domain::SourceRegionStatus::Complete,
                crate::domain::SourceRegionStatus::Complete,
            )
            .unwrap(),
            &[],
        )
        .unwrap();

    let mut finalized = false;
    for step in 0..32_i64 {
        match fixture.ledger.resume_usage_carry(1, 700 + step).unwrap() {
            CarryStepOutcome::Progress => {}
            CarryStepOutcome::FinalizedMissing => {
                finalized = true;
                break;
            }
            CarryStepOutcome::FinalizedPresent => {
                panic!("grown source unexpectedly finalized as present")
            }
        }
    }
    assert!(finalized, "grown source did not reach carry finalize");

    let connection = fixture.ledger.connection().unwrap();
    let proof: (String, Option<String>, String, i64, String, i64) = connection
        .query_row(
            "SELECT b.completion_status,b.completion_error_code,b.carry_phase,
                    c.committed_offset,c.processing_status,
                    s.observed_size
             FROM usage_build_sources b
             JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                AND c.consumer_kind='usage'
             JOIN source_files s ON s.source_file_id=b.source_file_id
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
                ))
            },
        )
        .unwrap();
    assert_eq!(
        proof,
        (
            "blocked".into(),
            Some("SOURCE_MISSING_WITH_UNVERIFIED_TAIL".into()),
            "none".into(),
            active_offset,
            "ready".into(),
            active_offset + 10,
        )
    );
}

#[test]
fn t_s04_050_carry_four_phase_mismatch_replaces_only_affected_member() {
    #[derive(Clone, Copy)]
    enum Mismatch {
        Generation,
        Inode,
        Binding,
        Guard,
    }
    for (target, mismatch) in [
        (UsageCarryPhase::Occurrences, Mismatch::Generation),
        (UsageCarryPhase::Turns, Mismatch::Inode),
        (UsageCarryPhase::Anomalies, Mismatch::Binding),
        (UsageCarryPhase::Finalize, Mismatch::Guard),
    ] {
        let (fixture, active_offset) = prepare_carry_fixture(2050, 1, 1);
        advance_carry_to(&fixture, target);
        let unaffected_before = source2_build_snapshot(&fixture);

        // Ensure the build epoch actually contains copied carry seed before
        // every replacement except the initial-occurrence boundary.
        if target != UsageCarryPhase::Occurrences {
            let connection = fixture.ledger.connection().unwrap();
            assert!(
                connection
                    .query_row(
                        "SELECT count(*) FROM usage_event_occurrences
                         WHERE ledger_epoch=2 AND source_file_id=1",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap()
                    > 0
            );
        }

        let (device, inode, root) = match mismatch {
            Mismatch::Generation => {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET file_generation=file_generation+1
                         WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
                (11, 11, "root")
            }
            Mismatch::Inode => {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET inode=99 WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
                (11, 99, "root")
            }
            Mismatch::Binding => {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET thread_id='other-root'
                         WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
                (11, 11, "other-root")
            }
            Mismatch::Guard => (11, 11, "root"),
        };

        let observations = vec![
            crate::domain::SourceObservation::new(
                fixture_path("usage-1.jsonl"),
                crate::domain::SourceArea::Sessions,
                device,
                inode,
                active_offset,
                1,
                900,
            )
            .unwrap(),
            crate::domain::SourceObservation::new(
                fixture_path("usage-2.jsonl"),
                crate::domain::SourceArea::Sessions,
                22,
                22,
                active_offset,
                1,
                900,
            )
            .unwrap(),
        ];
        let proof = crate::storage::source::UsageCarryObservationProof {
            device_id: device,
            inode,
            active_committed_offset: active_offset,
            guard_matches: !matches!(mismatch, Mismatch::Guard),
        };
        let outcome = fixture
            .ledger
            .record_source_observations_with_usage_carry_proofs(
                crate::domain::SourceObservationBatch::new(
                    observations,
                    crate::domain::SourceRegionStatus::Complete,
                    crate::domain::SourceRegionStatus::Complete,
                )
                .unwrap(),
                &[proof],
            )
            .unwrap();
        let affected = outcome
            .results
            .iter()
            .find(|result| result.source_file_id == 1)
            .unwrap();
        assert_eq!(
            affected.build_disposition,
            crate::domain::BuildDisposition::Replaced,
            "phase={target:?}, root={root}"
        );

        let connection = fixture.ledger.connection().unwrap();
        let reset: (String, Option<i64>, String, i64, i64, i64) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,
                        c.processing_status,c.committed_offset,
                        (SELECT count(*) FROM usage_event_occurrences
                         WHERE ledger_epoch=2 AND source_file_id=1),
                        (SELECT count(*) FROM usage_events
                         WHERE ledger_epoch=2 AND event_id=?1)
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
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
            reset,
            ("none".into(), None, "rebuild_required".into(), 0, 0, 0),
            "phase={target:?}"
        );
        drop(connection);
        assert_eq!(
            source2_build_snapshot(&fixture),
            unaffected_before,
            "unaffected member changed at phase {target:?}"
        );
    }
}

#[test]
fn t_s04_051_durable_carry_pages_resume_after_reopen_and_finalize_only_at_end() {
    const ROWS_PER_PHASE: i64 = 4097;
    let (fixture, active_offset) =
        prepare_carry_fixture(ROWS_PER_PHASE, ROWS_PER_PHASE, ROWS_PER_PHASE);

    // 4097 rows force three durable pages per copy phase. Reopen the
    // database before every page so both possible intermediate crash
    // boundaries in each phase resume only from the persisted cursor.
    for target in [
        UsageCarryPhase::Occurrences,
        UsageCarryPhase::Turns,
        UsageCarryPhase::Anomalies,
    ] {
        advance_carry_to(&fixture, target);
        let mut page_count = 0_i64;
        loop {
            let phase = carry_phase(&fixture);
            if phase != target {
                break;
            }

            let connection = fixture.ledger.connection().unwrap();
            let durable: (String, i64, String) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,b.completion_status
                     FROM source_checkpoints c JOIN usage_build_sources b
                       ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                durable,
                ("rebuild_required".into(), 0, "pending".into()),
                "phase {target:?}, page {page_count}"
            );
            drop(connection);

            let reopened = Ledger::open(LedgerOptions::new(
                fixture.root.join("mu.sqlite3"),
                fixture.root.join("codex"),
            ))
            .unwrap();
            reopened.resume_usage_carry(1, 1_000 + page_count).unwrap();
            drop(reopened);
            page_count += 1;
            assert!(page_count <= 4, "carry phase failed to advance");
        }
        assert_eq!(page_count, 3, "phase {target:?} must span three pages");
    }

    advance_carry_to(&fixture, UsageCarryPhase::Finalize);
    {
        let connection = fixture.ledger.connection().unwrap();
        let before_final: (String, i64, String, String) = connection
            .query_row(
                "SELECT c.processing_status,c.committed_offset,b.completion_status,b.carry_phase
                 FROM source_checkpoints c JOIN usage_build_sources b
                   ON b.source_file_id=c.source_file_id
                 WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            before_final,
            (
                "rebuild_required".into(),
                0,
                "pending".into(),
                "finalize".into()
            )
        );
    }
    let reopened = Ledger::open(LedgerOptions::new(
        fixture.root.join("mu.sqlite3"),
        fixture.root.join("codex"),
    ))
    .unwrap();
    assert_eq!(
        reopened.resume_usage_carry(1, 1_200).unwrap(),
        CarryStepOutcome::FinalizedMissing
    );

    let connection = fixture.ledger.connection().unwrap();
    let final_proof: (String, i64, String, String, i64, i64, i64) = connection
        .query_row(
            "SELECT c.processing_status,c.committed_offset,b.completion_status,b.carry_phase,
                    (SELECT count(*) FROM usage_event_occurrences
                     WHERE ledger_epoch=2 AND source_file_id=1),
                    (SELECT count(*) FROM turns
                     WHERE ledger_epoch=2 AND source_file_id=1),
                    (SELECT count(*) FROM ingest_anomalies
                     WHERE ledger_epoch=2 AND source_file_id=1)
             FROM source_checkpoints c JOIN usage_build_sources b
               ON b.source_file_id=c.source_file_id
             WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
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
        final_proof,
        (
            "ready".into(),
            active_offset,
            "carried".into(),
            "none".into(),
            ROWS_PER_PHASE,
            ROWS_PER_PHASE,
            ROWS_PER_PHASE
        )
    );
}
