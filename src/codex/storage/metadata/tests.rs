#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::codex::domain::{
        CheckpointProcessingStatus, ContinuationState, CwdProvenance, FactQualityStatus,
        MetadataCheckpointAdvance, MetadataCommitBatch, MetadataSourceCommit, MetadataThreadCommit,
        OwnershipConfidence, RolloutMetadataFact, SafeFactState,
    };
    use crate::codex::normalization::USAGE_PARSER_VERSION;
    use crate::codex::storage::rebuild::tests::RebuildLedger;
    use crate::codex::storage::{CodexStorage, CodexStorageError};
    use crate::domain::{
        AgentRole, MetadataQualityStatus, Patch, ProjectKind, ResolvedThreadPatch, ScanStartEvent,
        ScanTrigger, SessionIdentity,
    };
    use crate::source::SourceId;
    use crate::source::SourceStorage;
    use crate::storage::{Ledger, LedgerOptions};
    use rusqlite::Connection;

    mod spec01_storage_integration;

    fn thread_patch(thread_id: &str, resolved_at_ms: i64) -> ResolvedThreadPatch {
        let identity = SessionIdentity::new(thread_id, SourceId::CODEX, thread_id).unwrap();
        ResolvedThreadPatch::new(&identity, resolved_at_ms).expect("valid thread patch")
    }

    fn temp_paths(name: &str) -> (PathBuf, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("usagi-metadata-{name}-{stamp}"));
        fs::create_dir_all(&root).unwrap();
        (root.join("mu.sqlite3"), root.join("codex"))
    }

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-storage-metadata")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn source_fact_for(
        source_file_id: i64,
        file_generation: i64,
        offset: i64,
        owner: &str,
    ) -> RolloutMetadataFact {
        RolloutMetadataFact {
            source_file_id,
            file_generation,
            metadata_parser_version: 1,
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

    fn source_commit_for(
        source_file_id: i64,
        file_generation: i64,
        offset: i64,
        owner: &str,
        expected_previous_thread_id: Option<String>,
    ) -> MetadataSourceCommit {
        MetadataSourceCommit::new(
            source_file_id,
            file_generation,
            expected_previous_thread_id,
            owner,
            source_fact_for(source_file_id, file_generation, offset, owner),
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: offset,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap()
    }

    fn source_commit(expected_previous_thread_id: Option<String>) -> MetadataSourceCommit {
        source_commit_for(1, 1, 10, "thread", expected_previous_thread_id)
    }

    fn replay_regression_fact(continuation_state: ContinuationState) -> RolloutMetadataFact {
        RolloutMetadataFact {
            source_file_id: 1,
            file_generation: 1,
            metadata_parser_version: 1,
            resolved_through_offset: 10,
            owning_thread_id: "thread".to_owned(),
            continuation_state,
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
            replay_start_offset: Some(1),
            owning_records_start_offset: None,
            ownership_confidence: match continuation_state {
                ContinuationState::Unstable => OwnershipConfidence::Unresolved,
                ContinuationState::ReplayedAncestor | ContinuationState::OwningLive => {
                    OwnershipConfidence::Confirmed
                }
            },
            fact_quality_status: FactQualityStatus::Complete,
            relationship_conflict: false,
            updated_at_ms: 10,
        }
    }

    fn replay_regression_commit(continuation_state: ContinuationState) -> MetadataSourceCommit {
        MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            replay_regression_fact(continuation_state),
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .expect("assemble metadata regression source commit")
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

    fn insert_source(ledger: &Ledger) {
        let connection = ledger.connection().unwrap();
        connection
            .execute(
                "INSERT INTO codex_source_files (
                    source_file_id, thread_id, current_path, source_area,
                    device_id, inode, file_generation, observed_size,
                    observed_mtime_ns, file_status, last_seen_at_ms
                 ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                [fixture_path("rollout.jsonl")],
            )
            .unwrap();
    }

    #[test]
    fn replayed_ancestor_nonzero_metadata_checkpoint_is_storage_legal() {
        let (db, _home) = temp_paths("replayed-ancestor-positive");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        let group = MetadataThreadCommit::new(
            "thread",
            None,
            vec![replay_regression_commit(
                ContinuationState::ReplayedAncestor,
            )],
        )
        .unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .expect("replayed ancestor must be persistable at a nonzero checkpoint");

        let connection = Connection::open(&db).unwrap();
        let row: (Option<String>, String, i64, String) = connection
            .query_row(
                "SELECT sf.thread_id,f.continuation_state,
                        sc.committed_offset,sc.processing_status
                 FROM codex_source_files sf
                 JOIN codex_rollout_metadata_facts f USING (source_file_id)
                 JOIN codex_source_checkpoints sc USING (source_file_id)
                 WHERE sf.source_file_id=1 AND sc.consumer_kind='metadata'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                Some("thread".to_owned()),
                "replayed_ancestor".to_owned(),
                10,
                "ready".to_owned(),
            )
        );
    }

    #[test]
    fn unstable_nonzero_metadata_checkpoint_stays_rejected() {
        let (db, _home) = temp_paths("replayed-ancestor-negative");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        let group = MetadataThreadCommit::new(
            "thread",
            None,
            vec![replay_regression_commit(ContinuationState::Unstable)],
        )
        .unwrap();
        assert!(
            with_codex(&ledger, |storage| {
                storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
            })
            .is_err()
        );

        let connection = Connection::open(&db).unwrap();
        let durable: (Option<String>, i64, i64) = connection
            .query_row(
                "SELECT thread_id,
                        (SELECT count(*) FROM codex_rollout_metadata_facts WHERE source_file_id=1),
                        (SELECT count(*) FROM codex_source_checkpoints
                         WHERE source_file_id=1 AND consumer_kind='metadata')
                 FROM codex_source_files WHERE source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(durable, (None, 0, 0));
    }

    #[test]
    fn spec04_first_binding_reconciles_build_in_same_metadata_transaction() {
        let (db, home) = temp_paths("usage-binding-build");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO threads (
                        thread_id,source,native_session_id,parent_thread_id,root_session_id,
                        agent_role,project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                     ) VALUES ('thread','codex','thread',NULL,'thread','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE source_usage_epochs
                     SET active_epoch=1,active_parser_version=?1 WHERE source='codex'",
                    [USAGE_PARSER_VERSION],
                )
                .unwrap();
        }
        {
            let mut connection = ledger.connection().unwrap();
            RebuildLedger::new(&mut connection)
                .begin_or_resume(USAGE_PARSER_VERSION, &[1], 1)
                .unwrap();
        }
        {
            let connection = ledger.connection().unwrap();
            let frozen: (Option<String>, Option<String>) = connection
                .query_row(
                    "SELECT expected_owning_thread_id,expected_root_session_id
                     FROM codex_usage_build_sources WHERE build_epoch=2 AND source_file_id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(frozen, (None, None));
            // Inject a SQLite failure at the usage-build replacement boundary.
            // This is test-only fault injection: the metadata writes happen first
            // in the same transaction, so the trigger proves they roll back if
            // usage reconciliation cannot finish.
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_spec04_binding_reconcile
                     BEFORE DELETE ON codex_usage_build_sources
                     BEGIN
                       SELECT RAISE(ABORT, 'injected usage reconcile failure');
                     END;",
                )
                .unwrap();
        }

        let group = MetadataThreadCommit::new(
            "thread",
            None,
            vec![source_commit_for(1, 1, 10, "thread", None)],
        )
        .unwrap();
        assert!(
            with_codex(&ledger, |storage| storage.commit_metadata(
                MetadataCommitBatch::new(vec![group.clone()]).unwrap()
            ))
            .is_err()
        );
        {
            let connection = ledger.connection().unwrap();
            let rolled_back: (Option<String>, i64, Option<i64>) = connection
                .query_row(
                    "SELECT
                        (SELECT thread_id FROM codex_source_files WHERE source_file_id=1),
                        (SELECT count(*) FROM codex_rollout_metadata_facts WHERE source_file_id=1),
                        (SELECT committed_offset FROM codex_source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='metadata')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(rolled_back, (None, 0, None));
            connection
                .execute_batch("DROP TRIGGER fail_spec04_binding_reconcile;")
                .unwrap();
        }

        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        let connection = ledger.connection().unwrap();
        let committed: (
            Option<String>,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
        ) = connection
            .query_row(
                "SELECT
                        (SELECT thread_id FROM codex_source_files WHERE source_file_id=1),
                        (SELECT committed_offset FROM codex_source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='metadata'),
                        (SELECT processing_status FROM codex_source_checkpoints
                           WHERE source_file_id=1 AND consumer_kind='usage'),
                        (SELECT expected_owning_thread_id FROM codex_usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1),
                        (SELECT expected_root_session_id FROM codex_usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1),
                        (SELECT completion_status FROM codex_usage_build_sources
                           WHERE build_epoch=2 AND source_file_id=1)",
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
            committed,
            (
                Some("thread".to_owned()),
                10,
                "rebuild_required".to_owned(),
                Some("thread".to_owned()),
                Some("thread".to_owned()),
                "pending".to_owned(),
            )
        );
        let _ = fs::remove_file(db);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn commits_first_binding_fact_checkpoint_and_patch_atomically() {
        let (db, home) = temp_paths("commit");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        let binding_status: String = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT binding_status FROM codex_adapter_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding_status, "unbound");
        insert_source(&ledger);

        let mut patch = thread_patch("thread", 10);
        patch.agent_role = Patch::Set(AgentRole::Main);
        patch.title = Patch::Set("A title".to_owned());
        let group =
            MetadataThreadCommit::new("thread", Some(patch), vec![source_commit(None)]).unwrap();
        let outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(outcome.committed_group_count, 1);
        assert_eq!(outcome.data_revision, 1);
        assert!(outcome.data_changed);

        let connection = ledger.connection().unwrap();
        let binding: Option<String> = connection
            .query_row(
                "SELECT thread_id FROM codex_source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding.as_deref(), Some("thread"));
        let fact_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM codex_rollout_metadata_facts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fact_count, 1);
        let checkpoint_count: i64 = connection
            .query_row("SELECT count(*) FROM codex_source_checkpoints WHERE source_file_id = 1 AND consumer_kind = 'metadata'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(checkpoint_count, 1);
        let title: String = connection
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "A title");
    }

    #[test]
    fn metadata_fact_ordering_and_relationship_conflict_round_trip() {
        let (db, home) = temp_paths("fact-ordering");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        let mut fact = source_fact_for(1, 1, 10, "thread");
        fact.latest_context_model = Some("gpt-5.6-sol".to_owned());
        fact.latest_context_turn_id = Some("00000000-0000-7000-8000-000000000001".to_owned());
        fact.latest_context_at_ms = Some(42);
        fact.fact_quality_status = FactQualityStatus::Conflict;
        fact.relationship_conflict = true;
        let source = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            fact,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![source]).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();

        let state = with_codex(&ledger, |storage| storage.load_metadata_scan_state(&[1])).unwrap();
        let SafeFactState::Matching(fact) = &state.entries[0].safe_fact else {
            panic!("metadata fact should match after round trip");
        };
        assert_eq!(
            fact.latest_context_turn_id.as_deref(),
            Some("00000000-0000-7000-8000-000000000001")
        );
        assert_eq!(fact.latest_context_model.as_deref(), Some("gpt-5.6-sol"));
        assert!(fact.relationship_conflict);
        assert_eq!(fact.fact_quality_status, FactQualityStatus::Conflict);
    }

    #[test]
    fn source_only_repeat_does_not_advance_data_revision() {
        let (db, home) = temp_paths("source-only");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);
        let first = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        let first_outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![first]).unwrap())
        })
        .unwrap();
        assert_eq!(first_outcome.data_revision, 0);

        let second = MetadataThreadCommit::new(
            "thread",
            None,
            vec![source_commit(Some("thread".to_owned()))],
        )
        .unwrap();
        let second_outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![second]).unwrap())
        })
        .unwrap();
        assert_eq!(second_outcome.data_revision, 0);
        assert!(!second_outcome.data_changed);
    }

    #[test]
    fn stale_patch_rolls_back_source_and_checkpoint() {
        let (db, home) = temp_paths("rollback");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);
        let initial = MetadataThreadCommit::new(
            "thread",
            Some({
                let mut patch = thread_patch("thread", 20);
                patch.agent_role = Patch::Set(AgentRole::Main);
                patch
            }),
            vec![source_commit(None)],
        )
        .unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![initial]).unwrap())
        })
        .unwrap();

        let mut stale_patch = thread_patch("thread", 19);
        stale_patch.title = Patch::Set("must not write".to_owned());
        let stale = MetadataThreadCommit::new(
            "thread",
            Some(stale_patch),
            vec![source_commit(Some("thread".to_owned()))],
        )
        .unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![stale]).unwrap()))
            .is_err()
        );

        let connection = ledger.connection().unwrap();
        let title: Option<String> = connection
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, None);
        let offset: i64 = connection
            .query_row("SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id = 1 AND consumer_kind = 'metadata'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(offset, 10);
    }

    #[test]
    fn patch_only_creates_and_reapplies_a_thread_without_sources() {
        let (db, home) = temp_paths("patch-only");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        let mut patch = thread_patch("thread", 10);
        patch.agent_role = Patch::Set(AgentRole::Main);
        patch.title = Patch::Set("title".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(patch), Vec::new()).unwrap();
        let outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(outcome.data_revision, 1);

        let mut no_change = thread_patch("thread", 10);
        no_change.agent_role = Patch::Keep;
        let group = MetadataThreadCommit::new("thread", Some(no_change), Vec::new()).unwrap();
        let outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(outcome.data_revision, 1);
        assert!(!outcome.data_changed);
    }

    #[test]
    fn relationship_resolution_allows_missing_parent_and_root_rows() {
        let (db, home) = temp_paths("relationship-missing");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        let mut patch = thread_patch("child", 10);
        patch.agent_role = Patch::Set(AgentRole::Subagent);
        patch.parent_thread_id = Patch::Set("parent-not-yet-seen".to_owned());
        patch.root_session_id = Patch::Set("root-not-yet-seen".to_owned());
        let group = MetadataThreadCommit::new("child", Some(patch), Vec::new()).unwrap();

        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();

        let relationships: (Option<String>, Option<String>) = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT parent_thread_id, root_session_id
                 FROM threads WHERE thread_id = 'child'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            relationships,
            (
                Some("parent-not-yet-seen".to_owned()),
                Some("root-not-yet-seen".to_owned()),
            )
        );
    }

    #[test]
    fn relationship_resolution_rejects_existing_cross_source_parent() {
        let (db, home) = temp_paths("relationship-cross-source");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        ledger
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO threads (
                    thread_id, source, native_session_id, parent_thread_id, root_session_id,
                    agent_role, project_kind, archived, metadata_quality_status,
                    metadata_resolved_at_ms
                 ) VALUES ('other-parent', 'other', 'other-parent', NULL, 'other-parent',
                           'main', 'unknown', 0, 'complete', 1)",
                [],
            )
            .unwrap();

        let mut patch = thread_patch("child", 10);
        patch.agent_role = Patch::Set(AgentRole::Subagent);
        patch.parent_thread_id = Patch::Set("other-parent".to_owned());
        let group = MetadataThreadCommit::new("child", Some(patch), Vec::new()).unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()))
            .is_err()
        );
        let child_count: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM threads WHERE thread_id = 'child'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(child_count, 0);
    }

    #[test]
    fn t_s01_002_project_kind_is_stable_metadata_and_preserves_project_facts() {
        let (db, home) = temp_paths("project-kind");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();

        let mut initial = thread_patch("thread", 10);
        initial.agent_role = Patch::Set(AgentRole::Main);
        initial.project_name = Patch::Set("Existing name".to_owned());
        initial.project_path = Patch::Set(fixture_path("existing-project"));
        initial.project_kind = Patch::Set(ProjectKind::Project);
        let group = MetadataThreadCommit::new("thread", Some(initial), Vec::new()).unwrap();
        let first = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(first.data_revision, 1);

        let mut projectless = thread_patch("thread", 11);
        projectless.project_kind = Patch::Set(ProjectKind::Projectless);
        let group = MetadataThreadCommit::new("thread", Some(projectless), Vec::new()).unwrap();
        let second = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(second.data_revision, 2);

        let mut unchanged = thread_patch("thread", 11);
        unchanged.project_kind = Patch::Set(ProjectKind::Projectless);
        let group = MetadataThreadCommit::new("thread", Some(unchanged), Vec::new()).unwrap();
        let third = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(third.data_revision, 2);
        assert!(!third.data_changed);

        let row: (String, String, String) = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT project_kind,project_path,project_name FROM threads WHERE thread_id='thread'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "projectless".to_owned(),
                fixture_path("existing-project"),
                "Existing name".to_owned()
            )
        );
        let projection = with_codex(&ledger, |storage| storage.load_existing_threads()).unwrap();
        assert_eq!(projection[0].project_kind, ProjectKind::Projectless);
        let _ = fs::remove_file(db);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn keep_set_and_full_resolution_clear_have_expected_effects() {
        let (db, home) = temp_paths("tri-state");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        let mut initial = thread_patch("thread", 10);
        initial.agent_role = Patch::Set(AgentRole::Main);
        initial.title = Patch::Set("old".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(initial), Vec::new()).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();

        let mut keep = thread_patch("thread", 11);
        keep.title = Patch::Keep;
        let group = MetadataThreadCommit::new("thread", Some(keep), Vec::new()).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        let title: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("old"));

        let mut set = thread_patch("thread", 12);
        set.title = Patch::Set("new".to_owned());
        let group = MetadataThreadCommit::new("thread", Some(set), Vec::new()).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();

        let mut clear = thread_patch("thread", 13).full_resolution(true);
        clear.title = Patch::Clear;
        let group = MetadataThreadCommit::new("thread", Some(clear), Vec::new()).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        let title: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, None);

        let mut illegal_clear = thread_patch("thread", 14);
        illegal_clear.title = Patch::Clear;
        let group = MetadataThreadCommit::new("thread", Some(illegal_clear), Vec::new());
        assert!(group.is_err());
    }

    #[test]
    fn usage_checkpoint_is_not_modified_by_metadata_commit() {
        let (db, home) = temp_paths("usage-preserved");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);
        ledger
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO codex_source_checkpoints (
                    source_file_id, consumer_kind, parser_version, committed_offset,
                    guard_hash, processing_status
                 ) VALUES (1, 'usage', 7, 5, x'AA', 'ready')",
                [],
            )
            .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        let checkpoint: (i64, i64, Vec<u8>) = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT parser_version, committed_offset, guard_hash
                 FROM codex_source_checkpoints
                 WHERE source_file_id = 1 AND consumer_kind = 'usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(checkpoint, (7, 5, vec![0xAA]));
    }

    #[test]
    fn cas_offset_provenance_and_continuation_fail_before_writes() {
        let (db, home) = temp_paths("preconditions");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        // Fact/checkpoint offsets must be equal even though the domain command
        // can be assembled before storage sees the source row.
        let mut mismatched_fact = source_fact_for(1, 1, 9, "thread");
        mismatched_fact.updated_at_ms = 9;
        let mismatch = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            mismatched_fact,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![mismatch]).unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()))
            .is_err()
        );

        let mut unstable = source_fact_for(1, 1, 10, "thread");
        unstable.continuation_state = ContinuationState::Unstable;
        unstable.ownership_confidence = OwnershipConfidence::Unresolved;
        let unstable = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            unstable,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        )
        .unwrap();
        let group = MetadataThreadCommit::new("thread", None, vec![unstable]).unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()))
            .is_err()
        );

        // First bind successfully, then stale expected_previous_thread_id is
        // rejected and cannot advance metadata a second time.
        let first = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![first]).unwrap())
        })
        .unwrap();
        let stale = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![stale]).unwrap()))
            .is_err()
        );
        let revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);
    }

    #[test]
    fn generation_id_and_provenance_conflicts_are_rejected() {
        let (db, home) = temp_paths("identity-conflicts");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        let generation_mismatch = source_commit_for(1, 2, 10, "thread", None);
        let group = MetadataThreadCommit::new("thread", None, vec![generation_mismatch]).unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()))
            .is_err()
        );

        let mut wrong_owner = source_fact_for(1, 1, 10, "other");
        wrong_owner.ownership_confidence = OwnershipConfidence::Unresolved;
        let wrong_owner = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            wrong_owner,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        );
        assert!(wrong_owner.is_err());

        let mut bad_provenance = source_fact_for(1, 1, 10, "thread");
        bad_provenance.cwd = Some(fixture_path("tmp"));
        bad_provenance.cwd_provenance = Some(CwdProvenance::SessionMeta);
        bad_provenance.cwd_record_offset = None;
        let bad_provenance = MetadataSourceCommit::new(
            1,
            1,
            None,
            "thread",
            bad_provenance,
            MetadataCheckpointAdvance {
                parser_version: 1,
                committed_offset: 10,
                guard_hash: Some(vec![1]),
                processing_status: CheckpointProcessingStatus::Ready,
                last_successful_scan_at_ms: Some(10),
                last_error_code: None,
            },
        );
        assert!(bad_provenance.is_err());

        let mut bad_patch = thread_patch("other", 1);
        bad_patch.title = Patch::Set("mismatch".to_owned());
        assert!(MetadataThreadCommit::new("thread", Some(bad_patch), Vec::new()).is_err());
    }

    #[test]
    fn source_changed_codex_home_blocks_metadata_writes() {
        let (db, home_a) = temp_paths("source-ready");
        let ledger_a = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger_a);
        let group = MetadataThreadCommit::new("thread", None, vec![source_commit(None)]).unwrap();
        ledger_a
            .connection()
            .unwrap()
            .execute(
                "UPDATE codex_adapter_state
                 SET home_fingerprint='different',binding_status='source_changed'
                 WHERE id=1",
                [],
            )
            .unwrap();
        let reopened = Arc::new(Ledger::open(LedgerOptions::new(&db)).unwrap());
        let source = SourceStorage::with_ledger("test", SourceId::CODEX, reopened);
        let storage = CodexStorage::new(&source).unwrap();
        assert!(matches!(
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()),
            Err(CodexStorageError::BindingSourceChanged)
        ));
        let binding: Option<String> = ledger_a
            .connection()
            .unwrap()
            .query_row(
                "SELECT thread_id FROM codex_source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding, None);
    }

    #[test]
    fn t_s05_014_015_revision_watch_publishes_only_postcommit_and_coalesces_latest_tuple() {
        let (db, home) = temp_paths("spec05-revision-watch");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        insert_source(&ledger);

        let mut receiver = ledger.subscribe_revisions();
        assert!(!receiver.has_changed().unwrap());

        // A status-only commit is published after its SQLite transaction commits.
        ledger
            .mark_scan_started(
                ScanStartEvent::new(
                    "00000000-0000-4000-8000-000000000501",
                    ScanTrigger::Manual,
                    10,
                )
                .unwrap(),
            )
            .unwrap();
        // Do not consume the watch notification yet.  The following data-only
        // metadata commit must coalesce with it into the latest revision tuple.
        let mut patch = thread_patch("thread", 10);
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group =
            MetadataThreadCommit::new("thread", Some(patch), vec![source_commit(None)]).unwrap();
        let outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(outcome.data_revision, 1);
        // The helper opens an independent Ledger handle, so bridge the
        // process-local revision notification onto the handle subscribed
        // above after the committed tuple has been read from SQLite.
        ledger.publish_revisions(
            outcome.data_revision,
            ledger.app_state().unwrap().scan.status_revision,
        );

        assert!(receiver.has_changed().unwrap());
        let latest = *receiver.borrow_and_update();
        assert_eq!(latest.data_revision, 1);
        assert_eq!(latest.status_revision, 1);
        assert!(!receiver.has_changed().unwrap());

        // A failing metadata transaction must not publish a revision that was
        // never committed.  Reusing the stale expected previous binding makes
        // the production CAS fail before any durable write.
        let mut stale_patch = thread_patch("thread", 20);
        stale_patch.title = Patch::Set("should-not-commit".to_owned());
        let stale_group =
            MetadataThreadCommit::new("thread", Some(stale_patch), vec![source_commit(None)])
                .unwrap();
        assert!(
            with_codex(&ledger, |storage| storage.commit_metadata(
                MetadataCommitBatch::new(vec![stale_group]).unwrap()
            ))
            .is_err()
        );
        assert_eq!(ledger.current_revision(), latest);
        assert!(!receiver.has_changed().unwrap());
    }

    #[test]
    fn multiple_sources_commit_as_one_group_and_rollback_together() {
        let (db, home) = temp_paths("multi-source");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-a.jsonl")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (2, NULL, ?1, 'archived_sessions', 1, 3, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-b.jsonl")],
                )
                .unwrap();
        }
        let mut patch = thread_patch("thread", 10);
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group = MetadataThreadCommit::new(
            "thread",
            Some(patch),
            vec![
                source_commit_for(1, 1, 10, "thread", None),
                source_commit_for(2, 1, 10, "thread", None),
            ],
        )
        .unwrap();
        let outcome = with_codex(&ledger, |storage| {
            storage.commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap())
        })
        .unwrap();
        assert_eq!(outcome.data_revision, 1);
        let binding_count: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM codex_source_files WHERE thread_id = 'thread'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding_count, 2);

        let (db, home) = temp_paths("multi-source-rollback");
        let ledger = Ledger::open(LedgerOptions::new(&db)).unwrap();
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (1, NULL, ?1, 'sessions', 1, 2, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-a.jsonl")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files (
                        source_file_id, thread_id, current_path, source_area,
                        device_id, inode, file_generation, observed_size,
                        observed_mtime_ns, file_status, last_seen_at_ms
                     ) VALUES (2, 'other', ?1, 'sessions', 1, 3, 1, 10, 0, 'present', 10)",
                    [fixture_path("rollout-b.jsonl")],
                )
                .unwrap();
        }
        let mut patch = thread_patch("thread", 10);
        patch.agent_role = Patch::Set(AgentRole::Main);
        let group = MetadataThreadCommit::new(
            "thread",
            Some(patch),
            vec![
                source_commit_for(1, 1, 10, "thread", None),
                source_commit_for(2, 1, 10, "thread", None),
            ],
        )
        .unwrap();
        assert!(
            with_codex(&ledger, |storage| storage
                .commit_metadata(MetadataCommitBatch::new(vec![group]).unwrap()))
            .is_err()
        );
        let binding: Option<String> = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT thread_id FROM codex_source_files WHERE source_file_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding, None);
        let revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);
        let fact_count: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM codex_rollout_metadata_facts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fact_count, 0);
    }
}
