#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;
    use rusqlite::params;

    use super::*;
    use crate::codex::domain::{
        BuildDisposition, CheckpointRebuildCommand, ConsumerKind, SafeFactMismatchReason,
        SafeFactState, SourceArea, SourceObservation, SourceObservationBatch,
        SourceObservationResult, SourceRegionStatus,
    };
    use crate::codex::normalization::{USAGE_PARSER_VERSION, canonical_algorithm_for};
    use crate::codex::storage::CodexStorage;
    use crate::codex::storage::rebuild::tests::RebuildLedger;
    use crate::codex::storage::source_state::UsageCarryObservationProof;
    use crate::source::{SourceId, SourceStorage};
    use crate::storage::{Ledger, LedgerOptions};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-storage-source")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn test_ledger() -> (Ledger, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "usagi-source-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let root = root.join(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
                .to_string(),
        );
        std::fs::create_dir_all(&root).expect("create temporary test directory");
        let ledger = Ledger::open(LedgerOptions::new(root.join("mu.sqlite3")))
            .expect("open temporary ledger");
        (ledger, root)
    }

    fn with_codex<T>(ledger: &Ledger, operation: impl FnOnce(&CodexStorage<'_>) -> T) -> T {
        let reopened = Arc::new(
            Ledger::open(LedgerOptions::new(ledger.database_path()))
                .expect("open Codex test storage"),
        );
        {
            let connection = reopened.connection().expect("open Codex test connection");
            connection
                .execute(
                    "UPDATE codex_adapter_state
                     SET home_fingerprint='test-fixture',binding_status='ready'
                     WHERE id=1",
                    [],
                )
                .expect("bind Codex test storage");
        }
        let source = SourceStorage::with_ledger("test", SourceId::CODEX, reopened);
        let storage = CodexStorage::new(&source).expect("create Codex test storage");
        operation(&storage)
    }

    fn observation(
        path: &str,
        area: SourceArea,
        device_id: i64,
        inode: i64,
        size: i64,
        mtime: i64,
        seen_at: i64,
    ) -> SourceObservation {
        SourceObservation::new(path, area, device_id, inode, size, mtime, seen_at)
            .expect("valid source observation")
    }

    fn one_observation(ledger: &Ledger, value: SourceObservation) -> SourceObservationResult {
        with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![value]),
                &[],
            )
        })
        .unwrap()
        .results
        .into_iter()
        .next()
        .unwrap()
    }

    fn complete_batch(observations: Vec<SourceObservation>) -> SourceObservationBatch {
        SourceObservationBatch::new(
            observations,
            SourceRegionStatus::Complete,
            SourceRegionStatus::Complete,
        )
        .unwrap()
    }

    fn cleanup(root: std::path::PathBuf) {
        std::fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[test]
    fn move_keeps_identity_then_replacement_resets_all_consumers() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/rollout.jsonl"),
                SourceArea::Sessions,
                10,
                20,
                100,
                1_000,
                1,
            ),
        );
        assert!(first.created);
        assert_eq!(first.file_generation, 1);
        let source_file_id = first.source_file_id;

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "INSERT INTO codex_source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 4, 88, X'02', 'ready')",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET parser_version = 3, committed_offset = 77,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        let moved = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-archive/rollout.jsonl"),
                SourceArea::ArchivedSessions,
                10,
                20,
                100,
                1_000,
                2,
            ),
        );
        assert_eq!(moved.source_file_id, source_file_id);
        assert_eq!(moved.file_generation, 1);
        assert!(moved.moved);
        assert!(!moved.replaced);

        let connection = Connection::open(ledger.database_path()).unwrap();
        let (path, metadata_offset, usage_offset): (String, i64, i64) = connection
            .query_row(
                "SELECT s.current_path,
                        (SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id = s.source_file_id AND consumer_kind = 'metadata'),
                        (SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id = s.source_file_id AND consumer_kind = 'usage')
                 FROM codex_source_files s WHERE source_file_id = ?1",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, fixture_path("mu-archive/rollout.jsonl"));
        assert_eq!(metadata_offset, 77);
        assert_eq!(usage_offset, 88);
        drop(connection);

        let rewritten = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-archive/rollout.jsonl"),
                SourceArea::ArchivedSessions,
                10,
                20,
                100,
                1_001,
                3,
            ),
        );
        assert_eq!(rewritten.source_file_id, source_file_id);
        assert_eq!(rewritten.file_generation, 2);
        assert!(rewritten.replaced);
        assert_eq!(rewritten.build_disposition, BuildDisposition::Unchanged);
        assert_eq!(
            rewritten.rebuild_consumers,
            vec![ConsumerKind::Metadata, ConsumerKind::Usage]
        );

        let connection = Connection::open(ledger.database_path()).unwrap();
        let rows: Vec<(String, i64, Option<Vec<u8>>)> = connection
            .prepare(
                "SELECT consumer_kind, committed_offset, guard_hash
                 FROM codex_source_checkpoints WHERE source_file_id = ?1
                 ORDER BY consumer_kind",
            )
            .unwrap()
            .query_map([source_file_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("metadata".into(), 0, None), ("usage".into(), 0, None)]
        );
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn metadata_fact_matching_and_stale_states_are_explicit() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/fact.jsonl"),
                SourceArea::Sessions,
                3,
                4,
                64,
                2,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id = 'thread-1' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET parser_version = 7, committed_offset = 64,
                     guard_hash = X'07', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_rollout_metadata_facts (
                    source_file_id, file_generation, metadata_parser_version,
                    resolved_through_offset, owning_thread_id, continuation_state,
                    ownership_confidence, fact_quality_status, updated_at_ms
                 ) VALUES (?1, 1, 7, 64, 'thread-1', 'owning_live', 'confirmed', 'complete', 8)",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        let state = with_codex(&ledger, |storage| {
            storage.load_metadata_scan_state(&[source_file_id])
        })
        .unwrap();
        assert!(matches!(
            state.entries[0].safe_fact,
            SafeFactState::Matching(_)
        ));

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_rollout_metadata_facts
                 SET metadata_parser_version = 6 WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        drop(connection);
        let state = with_codex(&ledger, |storage| {
            storage.load_metadata_scan_state(&[source_file_id])
        })
        .unwrap();
        assert_eq!(
            state.entries[0].safe_fact,
            SafeFactState::Stale(SafeFactMismatchReason::ParserVersionMismatch)
        );

        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_rollout_metadata_facts
                 SET metadata_parser_version = 7 WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET file_status = 'missing' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        drop(connection);
        let state = with_codex(&ledger, |storage| {
            storage.load_metadata_scan_state(&[source_file_id])
        })
        .unwrap();
        assert_eq!(
            state.entries[0].safe_fact,
            SafeFactState::Stale(SafeFactMismatchReason::SourceMissing)
        );
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn checkpoint_rebuild_isolated_to_requested_consumer() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/rebuild.jsonl"),
                SourceArea::Sessions,
                5,
                6,
                80,
                1,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET parser_version = 1, committed_offset = 80,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 2, 70, X'02', 'ready')",
                [source_file_id],
            )
            .unwrap();
        drop(connection);

        with_codex(&ledger, |storage| {
            storage.require_checkpoint_rebuild(
                CheckpointRebuildCommand::new(ConsumerKind::Metadata, vec![source_file_id])
                    .unwrap(),
            )
        })
        .unwrap();
        let connection = Connection::open(ledger.database_path()).unwrap();
        let metadata: (i64, Option<Vec<u8>>, String) = connection
            .query_row(
                "SELECT committed_offset, guard_hash, processing_status
                 FROM codex_source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let usage: (i64, Option<Vec<u8>>, String) = connection
            .query_row(
                "SELECT committed_offset, guard_hash, processing_status
                 FROM codex_source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'usage'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(metadata, (0, None, "rebuild_required".into()));
        assert_eq!(usage, (70, Some(vec![2]), "ready".into()));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn missing_source_restores_by_identity_at_new_path_without_reset() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/old.jsonl"),
                SourceArea::Sessions,
                9,
                10,
                20,
                1,
                1,
            ),
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET parser_version = 3, committed_offset = 20,
                     guard_hash = X'03', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [first.source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_source_checkpoints (
                    source_file_id, consumer_kind, parser_version,
                    committed_offset, guard_hash, processing_status
                 ) VALUES (?1, 'usage', 4, 19, X'04', 'ready')",
                [first.source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET file_status = 'missing' WHERE source_file_id = ?1",
                [first.source_file_id],
            )
            .unwrap();
        drop(connection);

        let restored = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/new.jsonl"),
                SourceArea::Sessions,
                9,
                10,
                20,
                1,
                2,
            ),
        );
        assert!(!restored.created);
        assert!(restored.moved);
        assert!(!restored.replaced);
        assert_eq!(restored.source_file_id, first.source_file_id);
        assert_eq!(restored.file_generation, 1);
        assert!(restored.rebuild_consumers.is_empty());

        let connection = Connection::open(ledger.database_path()).unwrap();
        let source: (i64, String, String) = connection
            .query_row(
                "SELECT file_generation, current_path, file_status
                 FROM codex_source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            source,
            (1, fixture_path("mu-sessions/new.jsonl"), "present".into())
        );
        let checkpoints: Vec<(String, i64, String)> = connection
            .prepare(
                "SELECT consumer_kind, committed_offset, processing_status
                 FROM codex_source_checkpoints WHERE source_file_id = ?1
                 ORDER BY consumer_kind",
            )
            .unwrap()
            .query_map([first.source_file_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            checkpoints,
            vec![
                ("metadata".into(), 20, "ready".into()),
                ("usage".into(), 19, "ready".into())
            ]
        );
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn two_sources_can_swap_paths_by_physical_identity() {
        let (ledger, root) = test_ledger();
        let initial = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/a.jsonl"),
                        SourceArea::Sessions,
                        21,
                        31,
                        50,
                        1,
                        1,
                    ),
                    observation(
                        &fixture_path("mu-sessions/b.jsonl"),
                        SourceArea::Sessions,
                        22,
                        32,
                        60,
                        2,
                        1,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        let a_id = initial.results[0].source_file_id;
        let b_id = initial.results[1].source_file_id;

        let swapped = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/b.jsonl"),
                        SourceArea::Sessions,
                        21,
                        31,
                        50,
                        1,
                        2,
                    ),
                    observation(
                        &fixture_path("mu-sessions/a.jsonl"),
                        SourceArea::Sessions,
                        22,
                        32,
                        60,
                        2,
                        2,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        assert_eq!(swapped.results[0].source_file_id, a_id);
        assert_eq!(swapped.results[1].source_file_id, b_id);
        for result in &swapped.results {
            assert!(result.moved);
            assert!(!result.replaced);
            assert_eq!(result.file_generation, 1);
            assert!(result.rebuild_consumers.is_empty());
        }

        let connection = Connection::open(ledger.database_path()).unwrap();
        let a_path: String = connection
            .query_row(
                "SELECT current_path FROM codex_source_files WHERE source_file_id = ?1",
                [a_id],
                |row| row.get(0),
            )
            .unwrap();
        let b_path: String = connection
            .query_row(
                "SELECT current_path FROM codex_source_files WHERE source_file_id = ?1",
                [b_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_path, fixture_path("mu-sessions/b.jsonl"));
        assert_eq!(b_path, fixture_path("mu-sessions/a.jsonl"));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn moving_source_and_new_file_at_old_path_are_resolved_as_a_batch() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/a-slot.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                30,
                1,
                1,
            ),
        );

        // Put the new file first to prove matching is based on the complete
        // batch rather than observation iteration order.
        let outcome = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/a-slot.jsonl"),
                        SourceArea::Sessions,
                        42,
                        52,
                        10,
                        2,
                        2,
                    ),
                    observation(
                        &fixture_path("mu-sessions/b-slot.jsonl"),
                        SourceArea::Sessions,
                        41,
                        51,
                        30,
                        1,
                        2,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        let new_source = &outcome.results[0];
        let moved_source = &outcome.results[1];
        assert!(new_source.created);
        assert_ne!(new_source.source_file_id, first.source_file_id);
        assert_eq!(moved_source.source_file_id, first.source_file_id);
        assert!(moved_source.moved);
        assert!(!moved_source.replaced);
        assert_eq!(moved_source.file_generation, 1);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn safe_fact_record_offsets_cannot_exceed_resolved_offset() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/offsets.jsonl"),
                SourceArea::Sessions,
                61,
                71,
                64,
                1,
                1,
            ),
        );
        let source_file_id = result.source_file_id;
        let project_path = fixture_path("project");
        let connection = Connection::open(ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE codex_source_files SET thread_id = 'thread-offsets' WHERE source_file_id = ?1",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET parser_version = 1, committed_offset = 64,
                     guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_file_id],
            )
            .unwrap();
        connection
            .execute(
                &format!(
                    "INSERT INTO codex_rollout_metadata_facts (
                    source_file_id, file_generation, metadata_parser_version,
                    resolved_through_offset, owning_thread_id, continuation_state,
                    cwd, cwd_provenance, cwd_record_offset,
                    parent_thread_id_hint, parent_hint_provenance, parent_hint_record_offset,
                    agent_role_hint, agent_role_provenance, agent_role_record_offset,
                    replay_start_offset, owning_records_start_offset,
                    ownership_confidence, fact_quality_status, updated_at_ms
                 ) VALUES (
                    ?1, 1, 1, 64, 'thread-offsets', 'owning_live',
                    '{project_path}', 'session_meta', 1,
                    'parent', 'subagent_source', 2,
                    'worker', 'subagent_source', 3,
                    4, 5, 'confirmed', 'complete', 6
                 )"
                ),
                [source_file_id],
            )
            .unwrap();

        for column in [
            "cwd_record_offset",
            "parent_hint_record_offset",
            "agent_role_record_offset",
            "replay_start_offset",
            "owning_records_start_offset",
        ] {
            connection
                .execute(
                    &format!(
                        "UPDATE codex_rollout_metadata_facts SET {column} = 65 WHERE source_file_id = ?1"
                    ),
                    [source_file_id],
                )
                .unwrap();
            let state = with_codex(&ledger, |storage| {
                storage.load_metadata_scan_state(&[source_file_id])
            })
            .unwrap();
            assert_eq!(
                state.entries[0].safe_fact,
                SafeFactState::Stale(SafeFactMismatchReason::InvalidFact),
                "column {column}"
            );
            connection
                .execute(
                    &format!(
                        "UPDATE codex_rollout_metadata_facts SET {column} = 1 WHERE source_file_id = ?1"
                    ),
                    [source_file_id],
                )
                .unwrap();
        }
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn rebuild_rejects_missing_consumer_checkpoint() {
        let (ledger, root) = test_ledger();
        let result = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/no-usage.jsonl"),
                SourceArea::Sessions,
                81,
                91,
                10,
                1,
                1,
            ),
        );
        let outcome = with_codex(&ledger, |storage| {
            storage.require_checkpoint_rebuild(
                CheckpointRebuildCommand::new(ConsumerKind::Usage, vec![result.source_file_id])
                    .unwrap(),
            )
        });
        assert!(outcome.is_err());

        let metadata_status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT processing_status FROM codex_source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [result.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(metadata_status, "pending");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn source_changed_rejects_source_observation_write() {
        let (ledger, root) = test_ledger();
        let changed = Ledger::open(LedgerOptions::new(ledger.database_path())).unwrap();
        changed
            .connection()
            .unwrap()
            .execute(
                "UPDATE codex_adapter_state
                 SET home_fingerprint='changed',binding_status='source_changed'
                 WHERE id=1",
                [],
            )
            .unwrap();
        let reopened = Arc::new(Ledger::open(LedgerOptions::new(ledger.database_path())).unwrap());
        let source = SourceStorage::with_ledger("test", SourceId::CODEX, Arc::clone(&reopened));
        let storage = CodexStorage::new(&source).unwrap();
        let outcome = storage.record_source_observations_with_usage_carry_proofs(
            complete_batch(vec![observation(
                &fixture_path("mu-sessions/rejected.jsonl"),
                SourceArea::Sessions,
                101,
                111,
                10,
                1,
                1,
            )]),
            &[],
        );
        assert!(outcome.is_err());
        let count: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM codex_source_files", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        drop(storage);
        drop(source);
        drop(reopened);
        drop(changed);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn unavailable_region_preserves_existing_source_status() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/unavailable.jsonl"),
                SourceArea::Sessions,
                11,
                12,
                20,
                1,
                1,
            ),
        );

        with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                SourceObservationBatch::new(
                    Vec::new(),
                    SourceRegionStatus::Unavailable("PERMISSION_DENIED".to_owned()),
                    SourceRegionStatus::Complete,
                )
                .unwrap(),
                &[],
            )
        })
        .unwrap();

        let status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT file_status FROM codex_source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "present");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn complete_region_marks_only_unobserved_present_sources_missing() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/complete-missing.jsonl"),
                SourceArea::Sessions,
                13,
                14,
                20,
                1,
                1,
            ),
        );

        with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                SourceObservationBatch::new(
                    Vec::new(),
                    SourceRegionStatus::Complete,
                    SourceRegionStatus::Unavailable("ARCHIVE_UNAVAILABLE".to_owned()),
                )
                .unwrap(),
                &[],
            )
        })
        .unwrap();

        let status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT file_status FROM codex_source_files WHERE source_file_id = ?1",
                [first.source_file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "missing");
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_build_observation_dispositions_are_atomic_and_preserve_required_boundary() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/spec04-a.jsonl"),
                SourceArea::Sessions,
                41,
                51,
                100,
                1_000,
                1,
            ),
        );
        let source1 = first.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root','codex','root',NULL,'root','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET thread_id='root' WHERE source_file_id=?1",
                    [source1],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            RebuildLedger::new(&mut connection)
                .begin_or_resume(USAGE_PARSER_VERSION, &[source1], 2)
                .unwrap();
            RebuildLedger::new(&mut connection)
                .record_progress(crate::codex::storage::rebuild::SourceProgress {
                    source_file_id: source1,
                    expected_generation: 1,
                    start_offset: 0,
                    last_complete_offset: 100,
                    observed_raw_size: 100,
                    expected_guard_hash: None,
                    guard_hash: Some(vec![7; 32]),
                    tail: crate::codex::storage::rebuild::TailProof::None,
                    updated_at_ms: 3,
                })
                .unwrap();
        }

        // Same physical generation and raw size retains the completed proof.
        let unchanged = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-a.jsonl"),
                    SourceArea::Sessions,
                    41,
                    51,
                    100,
                    1_000,
                    4,
                )]),
                &[],
            )
        })
        .unwrap();
        assert_eq!(
            unchanged.results[0].build_disposition,
            BuildDisposition::Unchanged
        );

        // Same-generation growth invalidates only completion/tail proof. The
        // reader-proven required boundary remains 100 until a reader commit.
        let grown = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-a.jsonl"),
                    SourceArea::Sessions,
                    41,
                    51,
                    120,
                    1_001,
                    5,
                )]),
                &[],
            )
        })
        .unwrap();
        assert_eq!(
            grown.results[0].build_disposition,
            BuildDisposition::CompletionInvalidated
        );
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let proof: (i64, i64, String, String, i64) = connection
                .query_row(
                    "SELECT b.required_through_offset,b.observed_raw_size,b.raw_tail_status,
                            b.completion_status,c.committed_offset
                     FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source1],
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
                proof,
                (100, 120, "unverified".into(), "pending".into(), 100)
            );
        }

        // A new present file is added to the frozen manifest in the same
        // source-observation transaction and receives the mandatory from-zero
        // usage checkpoint.
        let added = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/spec04-a.jsonl"),
                        SourceArea::Sessions,
                        41,
                        51,
                        120,
                        1_001,
                        6,
                    ),
                    observation(
                        &fixture_path("mu-sessions/spec04-b.jsonl"),
                        SourceArea::Sessions,
                        42,
                        52,
                        30,
                        2_000,
                        6,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        let source2_result = added.results.iter().find(|r| r.created).unwrap();
        assert_eq!(
            source2_result.build_disposition,
            BuildDisposition::MemberAdded
        );
        let source2 = source2_result.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let row: (i64, i64, String, i64, String) = connection
                .query_row(
                    "SELECT b.required_through_offset,b.observed_raw_size,b.raw_tail_status,
                            c.committed_offset,c.processing_status
                     FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source2],
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
                row,
                (0, 30, "unverified".into(), 0, "rebuild_required".into())
            );
        }

        // Carry-in-progress reappearance under the same frozen identity keeps
        // its cursor and is reported distinctly; source observation must not
        // restore the checkpoint early.
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=?1",
                    [source2],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_usage_build_sources SET carry_from_epoch=0,carry_phase='occurrences',
                         completion_status='pending',completion_error_code=NULL,carry_after_start_offset=7
                     WHERE build_epoch=1 AND source_file_id=?1",
                    [source2],
                )
                .unwrap();
        }
        let resumed = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/spec04-a.jsonl"),
                        SourceArea::Sessions,
                        41,
                        51,
                        120,
                        1_001,
                        7,
                    ),
                    observation(
                        &fixture_path("mu-sessions/spec04-b.jsonl"),
                        SourceArea::Sessions,
                        42,
                        52,
                        30,
                        2_000,
                        7,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        let resumed2 = resumed
            .results
            .iter()
            .find(|r| r.source_file_id == source2)
            .unwrap();
        assert_eq!(
            resumed2.build_disposition,
            BuildDisposition::CarryResumedPresent
        );
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let row: (String, Option<i64>, String, i64) = connection
                .query_row(
                    "SELECT carry_phase,carry_after_start_offset,c.processing_status,c.committed_offset
                     FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=1 AND b.source_file_id=?1",
                    [source2],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                row,
                ("occurrences".into(), Some(7), "rebuild_required".into(), 0)
            );
        }

        // Physical replacement invokes the shared replacement protocol. The
        // affected source restarts at zero while the other old manifest member
        // and its required boundary remain present.
        let replaced = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![
                    observation(
                        &fixture_path("mu-sessions/spec04-a.jsonl"),
                        SourceArea::Sessions,
                        141,
                        151,
                        20,
                        3_000,
                        8,
                    ),
                    observation(
                        &fixture_path("mu-sessions/spec04-b.jsonl"),
                        SourceArea::Sessions,
                        42,
                        52,
                        30,
                        2_000,
                        8,
                    ),
                ]),
                &[],
            )
        })
        .unwrap();
        let replaced1 = replaced
            .results
            .iter()
            .find(|r| r.current_generation() == 2)
            .unwrap();
        assert_eq!(replaced1.build_disposition, BuildDisposition::Replaced);
        let connection = Connection::open(ledger.database_path()).unwrap();
        let ids = connection
            .prepare("SELECT source_file_id FROM codex_usage_build_sources WHERE build_epoch=1 ORDER BY source_file_id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec![source1, source2]);
        let affected: (i64, i64, String) = connection
            .query_row(
                "SELECT b.required_generation,c.committed_offset,c.processing_status
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=?1",
                [source1],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(affected, (2, 0, "rebuild_required".into()));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_carry_present_active_prefix_guard_is_decided_inside_observation_transaction() {
        fn prepare() -> (Ledger, std::path::PathBuf, i64) {
            let (ledger, root) = test_ledger();
            let first = one_observation(
                &ledger,
                observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    1,
                ),
            );
            let source = first.source_file_id;
            {
                let connection = Connection::open(ledger.database_path()).unwrap();
                connection
                    .execute_batch(
                        "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root','codex','root',NULL,'root','main','unknown',0,'complete',1);",
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE codex_source_files SET thread_id='root' WHERE source_file_id=?1",
                        [source],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,
                            committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code)
                         VALUES (?1,'usage',?2,10,?3,'ready',1,NULL)",
                        rusqlite::params![source, USAGE_PARSER_VERSION, vec![9_u8; 32]],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO codex_usage_source_states(
                            ledger_epoch,source_file_id,file_generation,device_id,inode,
                            usage_parser_version,canonical_algorithm_version,resolved_through_offset,
                            observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,
                            root_session_id,continuation_state,chain_state,chain_block_reason,updated_at_ms)
                         VALUES (1,?1,1,61,71,?2,?3,10,10,'none',NULL,'root','root',
                                 'owning_live','continuous',NULL,1)",
                        rusqlite::params![
                            source,
                            USAGE_PARSER_VERSION,
                            crate::codex::normalization::USAGE_CANONICAL_ALGORITHM_VERSION,
                        ],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE source_usage_epochs
                         SET active_epoch=1,active_parser_version=?1,
                             build_epoch=NULL,build_parser_version=NULL
                         WHERE source='codex'",
                        [USAGE_PARSER_VERSION],
                    )
                    .unwrap();
            }
            {
                let mut connection = ledger.connection().unwrap();
                RebuildLedger::new(&mut connection)
                    .begin_or_resume(USAGE_PARSER_VERSION, &[source], 2)
                    .unwrap();
            }
            {
                let connection = Connection::open(ledger.database_path()).unwrap();
                connection
                    .execute(
                        "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=?1",
                        [source],
                    )
                    .unwrap();
            }
            with_codex(&ledger, |storage| storage.begin_carry(source, 3)).unwrap();
            (ledger, root, source)
        }

        let (ledger, root, source) = prepare();
        let requirement = with_codex(&ledger, |storage| {
            storage.load_usage_carry_observation_requirements()
        })
        .unwrap();
        assert_eq!(requirement.len(), 1);
        assert_eq!(requirement[0].active_committed_offset, 10);
        assert_eq!(requirement[0].active_guard_hash, Some(vec![9_u8; 32]));

        let outcome = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    4,
                )]),
                &[UsageCarryObservationProof {
                    device_id: 61,
                    inode: 71,
                    active_committed_offset: 10,
                    guard_matches: false,
                }],
            )
        })
        .unwrap();
        assert_eq!(
            outcome.results[0].build_disposition,
            BuildDisposition::Replaced
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        let row: (String, Option<i64>, String, i64, i64) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,c.processing_status,c.committed_offset,
                        (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=2 AND source_file_id=?1)
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(row, ("none".into(), None, "rebuild_required".into(), 0, 0));
        drop(connection);
        drop(ledger);
        cleanup(root);

        let (ledger, root, source) = prepare();
        let outcome = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-carry-guard.jsonl"),
                    SourceArea::Sessions,
                    61,
                    71,
                    10,
                    1_000,
                    4,
                )]),
                &[UsageCarryObservationProof {
                    device_id: 61,
                    inode: 71,
                    active_committed_offset: 10,
                    guard_matches: true,
                }],
            )
        })
        .unwrap();
        assert_eq!(
            outcome.results[0].build_disposition,
            BuildDisposition::CarryResumedPresent
        );
        let connection = Connection::open(ledger.database_path()).unwrap();
        let row: (String, String, i64) = connection
            .query_row(
                "SELECT b.carry_phase,c.processing_status,c.committed_offset
                 FROM codex_usage_build_sources b JOIN codex_source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("occurrences".into(), "rebuild_required".into(), 0));
        drop(connection);
        drop(ledger);
        cleanup(root);
    }

    #[test]
    fn spec04_source_observation_and_build_replacement_roll_back_together() {
        let (ledger, root) = test_ledger();
        let first = one_observation(
            &ledger,
            observation(
                &fixture_path("mu-sessions/spec04-atomic.jsonl"),
                SourceArea::Sessions,
                61,
                71,
                90,
                1_000,
                1,
            ),
        );
        let source_id = first.source_file_id;
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute(
                    "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                        metadata_quality_status,metadata_resolved_at_ms)
                     VALUES ('root','codex','root',NULL,'root','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE codex_source_files SET thread_id='root' WHERE source_file_id=?1",
                    [source_id],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            RebuildLedger::new(&mut connection)
                .begin_or_resume(USAGE_PARSER_VERSION, &[source_id], 2)
                .unwrap();
        }
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_spec04_source_build_replace
                     BEFORE DELETE ON codex_usage_build_sources
                     BEGIN
                       SELECT RAISE(ABORT, 'injected build replacement failure');
                     END;",
                )
                .unwrap();
        }

        let failed = with_codex(&ledger, |storage| {
            storage.record_source_observations_with_usage_carry_proofs(
                complete_batch(vec![observation(
                    &fixture_path("mu-sessions/spec04-atomic.jsonl"),
                    SourceArea::Sessions,
                    161,
                    171,
                    20,
                    2_000,
                    3,
                )]),
                &[],
            )
        });
        assert!(failed.is_err());
        {
            let connection = Connection::open(ledger.database_path()).unwrap();
            let source: (i64, i64, i64, i64, String) = connection
                .query_row(
                    "SELECT device_id,inode,file_generation,observed_size,file_status
                     FROM codex_source_files WHERE source_file_id=?1",
                    [source_id],
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
            assert_eq!(source, (61, 71, 1, 90, "present".into()));
            let build: (i64, i64, String) = connection
                .query_row(
                    "SELECT expected_file_generation,required_through_offset,completion_status
                     FROM codex_usage_build_sources WHERE build_epoch=1 AND source_file_id=?1",
                    [source_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(build, (1, 0, "pending".into()));
            let checkpoint: (i64, String) = connection
                .query_row(
                    "SELECT committed_offset,processing_status FROM codex_source_checkpoints
                     WHERE source_file_id=?1 AND consumer_kind='usage'",
                    [source_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(checkpoint, (0, "rebuild_required".into()));
            connection
                .execute_batch("DROP TRIGGER fail_spec04_source_build_replace;")
                .unwrap();
        }
        drop(ledger);
        cleanup(root);
    }
}
