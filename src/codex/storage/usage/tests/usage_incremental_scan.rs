use super::*;
use rusqlite::params;

fn commit_source(
    fixture: &Fixture,
    source_id: i64,
    device: i64,
    thread: &str,
    root: &str,
    id: char,
) {
    let mut source = source_commit(source_id, device, thread, root, id, false);
    let event_id = format!("{source_id:064x}");
    source.events[0].event_id = event_id.clone();
    source.occurrences[0].event_id = event_id;
    with_codex(&fixture.ledger, |storage| {
        storage.commit_group(batch(thread, root, source))
    })
    .unwrap();
}

fn set_idle_tail(
    fixture: &Fixture,
    source_id: i64,
    observed_size: i64,
    tail: &str,
    start: Option<i64>,
) {
    let connection = fixture.ledger.connection().unwrap();
    let committed = 20_i64;
    connection
        .execute(
            "UPDATE codex_source_files SET observed_size=?2 WHERE source_file_id=?1",
            params![source_id, observed_size],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE codex_source_checkpoints SET parser_version=?4,committed_offset=?2,
                guard_hash=?3,processing_status='ready',last_error_code=NULL
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            params![
                source_id,
                committed,
                vec![9_u8; 32],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE codex_usage_source_states SET observed_raw_size=?2,
                raw_tail_status=?3,raw_tail_start_offset=?4,
                resolved_through_offset=?5,previous_total_offset=?5,
                updated_at_ms=20 WHERE ledger_epoch=1 AND source_file_id=?1",
            params![source_id, observed_size, tail, start, committed],
        )
        .unwrap();
}

fn worklist_ids(fixture: &Fixture, source_ids: &[i64]) -> Vec<i64> {
    with_codex(&fixture.ledger, |storage| {
        storage.load_usage_work_list(
            source_ids,
            crate::codex::normalization::USAGE_PARSER_VERSION,
        )
    })
    .unwrap()
    .threads
    .into_iter()
    .flat_map(|thread| thread.source_file_ids)
    .collect()
}

#[test]
fn t_perf_001_stable_no_build_worklist_matrix() {
    let fixture = Fixture::new();
    for source_id in 1..=14 {
        let thread = match source_id {
            11 => Some("unresolved"),
            12 => None,
            _ => Some("child"),
        };
        fixture.add_source(source_id, thread, 100 + source_id);
    }

    for source_id in [1, 2, 3, 5, 7, 8, 9, 10, 13] {
        let mut source = source_commit(
            source_id,
            100 + source_id,
            "child",
            "root",
            char::from(b'a' + u8::try_from(source_id).unwrap()),
            source_id == 13,
        );
        let event_id = format!("{source_id:064x}");
        source.events[0].event_id = event_id.clone();
        source.occurrences[0].event_id = event_id;
        with_codex(&fixture.ledger, |storage| {
            storage.commit_group(batch("child", "root", source))
        })
        .unwrap_or_else(|error| {
            panic!(
                "source {source_id} commit failed: {error:?} ({})",
                std::error::Error::source(&error)
                    .map(ToString::to_string)
                    .unwrap_or_default()
            )
        });
    }

    set_idle_tail(&fixture, 1, 20, "none", None);
    set_idle_tail(&fixture, 2, 30, "half_line", Some(20));
    // The state still proves only the old fixed raw size, so a growth is a
    // candidate even though the previous tail was a valid half-line.
    set_idle_tail(&fixture, 3, 30, "half_line", Some(20));
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET observed_size=40 WHERE source_file_id=3",
            [],
        )
        .unwrap();
    set_idle_tail(&fixture, 5, 20, "none", None);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET file_generation=2 WHERE source_file_id=5",
            [],
        )
        .unwrap();
    set_idle_tail(&fixture, 9, 20, "none", None);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_usage_source_states SET usage_parser_version=3 WHERE ledger_epoch=1 AND source_file_id=9",
            [],
        )
        .unwrap();
    set_idle_tail(&fixture, 10, 20, "none", None);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET device_id=999,inode=999 WHERE source_file_id=10",
            [],
        )
        .unwrap();
    set_idle_tail(&fixture, 13, 20, "none", None);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_turns SET state_through_offset=21
             WHERE ledger_epoch=1 AND source_file_id=13 AND status='open'",
            [],
        )
        .unwrap();

    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_checkpoints SET processing_status='error'
             WHERE source_file_id=7 AND consumer_kind='usage'",
            [],
        )
        .unwrap();
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_checkpoints SET processing_status='rebuild_required'
             WHERE source_file_id=8 AND consumer_kind='usage'",
            [],
        )
        .unwrap();

    // A present, fully bound source with no Usage checkpoint at all is new
    // executable work, not an idle source under SQL NULL semantics.
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM codex_source_checkpoints
             WHERE source_file_id=14 AND consumer_kind='usage'",
            [],
        )
        .unwrap();

    let source_ids = (1..=14).collect::<Vec<_>>();
    assert_eq!(
        worklist_ids(&fixture, &source_ids),
        vec![3, 4, 5, 6, 7, 8, 9, 10, 13, 14]
    );

    // An unresolved relationship is not an executable Thread.  Once the
    // metadata binding is repaired, the same incomplete source is a candidate.
    assert!(!worklist_ids(&fixture, &[11, 12]).contains(&11));
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET thread_id='child' WHERE source_file_id=11",
            [],
        )
        .unwrap();
    assert_eq!(worklist_ids(&fixture, &[11, 12]), vec![11]);
}

#[test]
fn t_perf_002_build_global_control_worklist_matrix() {
    let epoch_zero = Fixture::new();
    epoch_zero.add_source(1, Some("child"), 11);
    epoch_zero
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE source_usage_epochs
             SET active_epoch=0,build_epoch=NULL,build_parser_version=NULL
             WHERE source='codex'",
            [],
        )
        .unwrap();
    let zero = with_codex(&epoch_zero.ledger, |storage| {
        storage.load_usage_work_list(&[1], crate::codex::normalization::USAGE_PARSER_VERSION)
    })
    .unwrap();
    assert_eq!(zero.epoch.active_epoch, 0);
    assert!(zero.threads.is_empty());

    let parser_mismatch = Fixture::new();
    parser_mismatch.add_source(1, Some("child"), 11);
    parser_mismatch
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE source_usage_epochs SET active_parser_version=3 WHERE source='codex'",
            [],
        )
        .unwrap();
    assert!(
        with_codex(&parser_mismatch.ledger, |storage| storage
            .load_usage_work_list(
                &[1],
                crate::codex::normalization::USAGE_PARSER_VERSION
            ))
        .unwrap()
        .threads
        .is_empty()
    );

    let fixture = Fixture::new();
    for source_id in 1..=3 {
        fixture.add_source(source_id, Some("child"), 10 + source_id);
    }
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=3",
            [],
        )
        .unwrap();
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
            "UPDATE codex_source_files SET file_status='present' WHERE source_file_id=3",
            [],
        )
        .unwrap();
    assert_eq!(worklist_ids(&fixture, &[1, 2, 3]), vec![1, 2, 3]);

    // A valid rebuilt proof is not detailed work; pending/carry members and
    // a present source missing from the manifest remain candidates.
    {
        let mut connection = fixture.ledger.connection().unwrap();
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
                updated_at_ms: 31,
            })
            .unwrap();
    }
    assert_eq!(worklist_ids(&fixture, &[1, 2, 3]), vec![1, 3]);
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_usage_build_sources SET carry_phase='occurrences',carry_from_epoch=1
             WHERE build_epoch=2 AND source_file_id=1",
            [],
        )
        .unwrap();
    assert_eq!(worklist_ids(&fixture, &[1, 2, 3]), vec![1, 3]);

    // A member without executable ownership/root is withheld from the Thread
    // queue; it is still present in the manifest and therefore not completion.
    fixture
        .ledger
        .connection()
        .unwrap()
        .execute(
            "UPDATE codex_source_files SET thread_id='unresolved' WHERE source_file_id=1",
            [],
        )
        .unwrap();
    assert_eq!(worklist_ids(&fixture, &[1, 2, 3]), vec![3]);
}

#[test]
fn t_perf_003_exact_detailed_plan_only_requested_sources() {
    let fixture = Fixture::new();
    fixture.add_source(1, Some("child"), 11);
    fixture.add_source(2, Some("other-root"), 12);
    fixture.add_source(3, Some("child"), 13);
    commit_source(&fixture, 1, 11, "child", "root", 'a');
    commit_source(&fixture, 2, 12, "other-root", "other-root", 'b');
    commit_source(&fixture, 3, 13, "child", "root", 'c');
    let expected = crate::domain::SourceUsageEpochState::new(
        crate::source::SourceId::CODEX,
        1,
        None,
        crate::codex::normalization::USAGE_PARSER_VERSION,
        None,
    )
    .unwrap();

    let exact = with_codex(&fixture.ledger, |storage| {
        storage.load_usage_scan_state_exact(
            &[3, 1],
            crate::codex::normalization::USAGE_PARSER_VERSION,
            expected.clone(),
        )
    })
    .unwrap();
    assert_eq!(
        exact
            .plans
            .iter()
            .map(|plan| plan.source_file_id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(exact.epoch, expected);

    let duplicate = with_codex(&fixture.ledger, |storage| {
        storage.load_usage_scan_state_exact(
            &[1, 1],
            crate::codex::normalization::USAGE_PARSER_VERSION,
            expected.clone(),
        )
    });
    assert!(duplicate.is_err());
    assert!(
        with_codex(&fixture.ledger, |storage| storage
            .load_usage_scan_state_exact(
                &[0],
                crate::codex::normalization::USAGE_PARSER_VERSION,
                expected.clone(),
            ))
        .is_err()
    );
    let stale_epoch = crate::domain::SourceUsageEpochState::new(
        crate::source::SourceId::CODEX,
        2,
        None,
        crate::codex::normalization::USAGE_PARSER_VERSION,
        None,
    )
    .unwrap();
    assert!(
        with_codex(&fixture.ledger, |storage| storage
            .load_usage_scan_state_exact(
                &[1],
                crate::codex::normalization::USAGE_PARSER_VERSION,
                stale_epoch
            ))
        .is_err()
    );

    {
        let mut connection = fixture.ledger.connection().unwrap();
        crate::codex::storage::rebuild::tests::RebuildLedger::new(&mut connection)
            .begin_or_resume(
                crate::codex::normalization::USAGE_PARSER_VERSION,
                &[1, 2, 3],
                40,
            )
            .unwrap();
    }
    let building = crate::domain::SourceUsageEpochState::new(
        crate::source::SourceId::CODEX,
        1,
        Some(2),
        crate::codex::normalization::USAGE_PARSER_VERSION,
        Some(crate::codex::normalization::USAGE_PARSER_VERSION),
    )
    .unwrap();
    let exact_build = with_codex(&fixture.ledger, |storage| {
        storage.load_usage_scan_state_exact(
            &[2],
            crate::codex::normalization::USAGE_PARSER_VERSION,
            building,
        )
    })
    .unwrap();
    assert_eq!(exact_build.plans.len(), 1);
    assert_eq!(exact_build.plans[0].source_file_id, 2);
}
