use super::*;
use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(crate) struct RebuildLedger<'connection> {
    connection: &'connection mut Connection,
}

fn replace_build_sources(
    transaction: &Connection,
    parser_version: i64,
    present: &BTreeSet<i64>,
    invalidated: &BTreeSet<i64>,
    now_ms: i64,
) -> Result<(), RebuildError> {
    let (active, build): (i64, Option<i64>) = transaction.query_row(
        "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some(build) = build else {
        return Err(RebuildError::Invalid("no usage build to replace"));
    };
    replace_build_preserving_all_members_tx(
        transaction,
        active,
        build,
        parser_version,
        present,
        invalidated,
        now_ms,
    )
}

impl<'connection> RebuildLedger<'connection> {
    pub(crate) fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }

    pub(crate) fn begin_or_resume(
        &mut self,
        target_parser_version: i64,
        present_source_ids: &[i64],
        now_ms: i64,
    ) -> Result<BuildSnapshot, RebuildError> {
        if crate::codex::normalization::canonical_algorithm_for(target_parser_version).is_none() {
            return Err(RebuildError::Invalid(
                "unknown usage parser canonical algorithm",
            ));
        }
        let present = present_source_ids.iter().copied().collect::<BTreeSet<_>>();
        let (active_epoch, build_epoch, build_parser): (i64, Option<i64>, Option<i64>) =
            self.connection.query_row(
                "SELECT active_epoch,build_epoch,build_parser_version
                 FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let next_build = build_epoch.unwrap_or(active_epoch + 1);
        if let (Some(build_epoch), Some(old_parser)) = (build_epoch, build_parser) {
            if old_parser != target_parser_version {
                let tx = self
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let changed = tx.execute(
                    "UPDATE source_usage_epochs SET build_parser_version=?1
                     WHERE source='codex' AND build_epoch=?2 AND build_parser_version=?3",
                    params![target_parser_version, build_epoch, old_parser],
                )?;
                if changed != 1 {
                    return Err(RebuildError::Cas("usage build parser retarget CAS failed"));
                }
                replace_target_preserving_members(
                    &tx,
                    active_epoch,
                    build_epoch,
                    target_parser_version,
                    &present,
                    now_ms,
                    true,
                )?;
                let snapshot = load_snapshot(&tx)?;
                tx.commit()?;
                return Ok(snapshot);
            }
        }
        if build_epoch.is_none() {
            self.connection.execute(
                "UPDATE source_usage_epochs SET build_epoch=?1,build_parser_version=?2
                 WHERE source='codex' AND build_epoch IS NULL",
                params![next_build, target_parser_version],
            )?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if build_epoch.is_none() {
            let active_epoch: i64 = tx.query_row(
                "SELECT active_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| row.get(0),
            )?;
            freeze_initial_members(
                &tx,
                active_epoch,
                next_build,
                target_parser_version,
                &present,
                now_ms,
            )?;
        }
        let snapshot = rebuild_begin_or_resume(&tx, target_parser_version, &present, now_ms)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn record_progress(
        &mut self,
        progress: SourceProgress,
    ) -> Result<ProgressOutcome, RebuildError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = rebuild_record_progress(&tx, &progress)?;
        tx.commit()?;
        Ok(outcome)
    }

    pub(crate) fn block_source(
        &mut self,
        source_file_id: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<(), RebuildError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        rebuild_block_source(&tx, source_file_id, error_code, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn retry_blocked(
        &mut self,
        source_file_id: i64,
        now_ms: i64,
    ) -> Result<(), RebuildError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        rebuild_retry_blocked(&tx, source_file_id, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn quarantine_session(
        &mut self,
        root_session_id: &str,
        error_code: &str,
        now_ms: i64,
    ) -> Result<usize, RebuildError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = rebuild_quarantine_session(&tx, root_session_id, error_code, now_ms)?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) fn replace_build_sources(
        &mut self,
        parser_version: i64,
        present_source_ids: &[i64],
        invalidated_source_ids: &[i64],
        now_ms: i64,
    ) -> Result<(), RebuildError> {
        let present = present_source_ids.iter().copied().collect::<BTreeSet<_>>();
        let invalidated = invalidated_source_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        replace_build_sources(&tx, parser_version, &present, &invalidated, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn activate(
        &mut self,
        complete_present_source_ids: &[i64],
    ) -> Result<ActivationOutcome, RebuildError> {
        let present = complete_present_source_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (build_epoch, target_parser) = current_build(&tx)?;
        verify_complete_present_set(&tx, build_epoch, &present)?;
        let unfinished: i64 = tx.query_row(
            "SELECT count(*) FROM codex_usage_build_sources
             WHERE build_epoch=?1 AND completion_status NOT IN ('rebuilt','carried','quarantined')",
            [build_epoch],
            |row| row.get(0),
        )?;
        if unfinished != 0 {
            return Err(RebuildError::Cas("manifest contains unfinished sources"));
        }
        let member_ids = query_ids(
            &tx,
            "SELECT source_file_id FROM codex_usage_build_sources
             WHERE build_epoch=?1 ORDER BY source_file_id",
            build_epoch,
        )?;
        for source_file_id in member_ids {
            let status: String = tx.query_row(
                "SELECT completion_status FROM codex_usage_build_sources
                 WHERE build_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_file_id],
                |row| row.get(0),
            )?;
            if status == "quarantined" {
                verify_quarantined_source(&tx, build_epoch, source_file_id)?;
            } else {
                verify_completion_row_for_storage(&tx, build_epoch, source_file_id)?;
            }
        }
        let (active_epoch, active_parser): (i64, i64) = tx.query_row(
            "SELECT active_epoch,active_parser_version FROM source_usage_epochs
             WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let visible_equal = canonical_visible_equal(&tx, active_epoch, build_epoch)?;
        tx.execute(
            "UPDATE source_usage_epochs SET active_epoch=?1,active_parser_version=?2,
                build_epoch=NULL,build_parser_version=NULL
             WHERE source='codex' AND build_epoch=?1 AND build_parser_version=?2",
            params![build_epoch, target_parser],
        )?;
        if !visible_equal {
            tx.execute(
                "UPDATE app_meta SET data_revision=data_revision+1 WHERE id=1",
                [],
            )?;
        }
        tx.execute(
            "DELETE FROM codex_usage_build_sources WHERE build_epoch=?1",
            [build_epoch],
        )?;
        let data_revision =
            tx.query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })?;
        tx.commit()?;
        Ok(ActivationOutcome {
            active_epoch: build_epoch,
            data_revision,
        })
    }
}

fn canonical_visible_equal(
    connection: &Connection,
    active_epoch: i64,
    build_epoch: i64,
) -> Result<bool, RebuildError> {
    let columns = "event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                   reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                   cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status";
    let sql = format!(
        "SELECT NOT EXISTS(
             SELECT {columns} FROM usage_events WHERE source='codex' AND source_epoch=?1
             EXCEPT SELECT {columns} FROM usage_events WHERE source='codex' AND source_epoch=?2
         ) AND NOT EXISTS(
             SELECT {columns} FROM usage_events WHERE source='codex' AND source_epoch=?2
             EXCEPT SELECT {columns} FROM usage_events WHERE source='codex' AND source_epoch=?1
         )"
    );
    Ok(
        connection.query_row(&sql, params![active_epoch, build_epoch], |row| {
            row.get::<_, i64>(0)
        })? != 0,
    )
}

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    connection
        .execute_batch(include_str!("../../../storage/schema/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../../storage/schema/0002_usage_ledger.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../../storage/schema/0003_normalized_token_usage.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../../storage/schema/0006_subagent_agent_path.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../../storage/schema/0007_usage_context_and_estimated_cost.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../../../storage/schema/0009_skill_usage_events.sql"
        ))
        .unwrap();
    connection
        .execute_batch(include_str!("legacy_rename.sql"))
        .unwrap();
    // The compact fixture intentionally skips the full session-resilience
    // migration, but rebuild activation visibility includes Codex
    // quarantine state. Keep the required sidecar tables faithful to the
    // production schema instead of teaching production code to tolerate a
    // missing table.
    connection
        .execute_batch(
            "CREATE TABLE codex_usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
                    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
                    primary_error_code TEXT NOT NULL CHECK (length(primary_error_code) > 0),
                    last_activity_at_ms INTEGER NOT NULL CHECK (last_activity_at_ms >= 0),
                    first_seen_at_ms INTEGER NOT NULL CHECK (first_seen_at_ms >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
                    PRIMARY KEY (ledger_epoch, root_session_id),
                    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id)
                 );
                 CREATE TABLE codex_usage_session_quarantine_sources (
                    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
                    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
                    source_file_id INTEGER NOT NULL CHECK (source_file_id > 0),
                    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
                    device_id INTEGER NOT NULL CHECK (device_id >= 0),
                    inode INTEGER NOT NULL CHECK (inode >= 0),
                    observed_size INTEGER NOT NULL CHECK (observed_size >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
                    PRIMARY KEY (ledger_epoch, root_session_id, source_file_id),
                    FOREIGN KEY (ledger_epoch, root_session_id)
                        REFERENCES codex_usage_session_quarantine(ledger_epoch, root_session_id)
                        ON DELETE CASCADE,
                    FOREIGN KEY (source_file_id) REFERENCES codex_source_files(source_file_id)
                 );",
        )
        .unwrap();
    // Promote the compact fixture to the v11 source-aware canonical
    // columns. The real migration performs this transformation before
    // runtime code executes; this local fixture keeps legacy insert
    // statements readable while mirroring their Codex source identity.
    connection
            .execute_batch(
                "ALTER TABLE usage_events ADD COLUMN source TEXT;
                 ALTER TABLE usage_events ADD COLUMN source_epoch INTEGER;
                 ALTER TABLE codex_usage_event_occurrences ADD COLUMN source TEXT;
                 CREATE TABLE source_usage_epochs(
                    source TEXT PRIMARY KEY, active_epoch INTEGER NOT NULL,
                    build_epoch INTEGER, active_parser_version INTEGER NOT NULL,
                    build_parser_version INTEGER
                 );
                 INSERT INTO source_usage_epochs(source,active_epoch,active_parser_version)
                    VALUES ('codex',0,0);
                 CREATE TRIGGER usage_events_v11_defaults AFTER INSERT ON usage_events
                 WHEN NEW.source IS NULL
                 BEGIN
                    UPDATE usage_events SET source='codex',source_epoch=NEW.ledger_epoch
                    WHERE rowid=NEW.rowid;
                 END;
                 CREATE TRIGGER usage_occurrences_v11_defaults AFTER INSERT ON codex_usage_event_occurrences
                 WHEN NEW.source IS NULL
                 BEGIN
                    UPDATE codex_usage_event_occurrences SET source='codex' WHERE rowid=NEW.rowid;
                 END;
                 CREATE TRIGGER app_meta_epoch_v11 AFTER UPDATE OF usage_active_epoch,
                    usage_build_epoch,usage_parser_version,usage_build_parser_version ON app_meta
                 BEGIN
                    UPDATE source_usage_epochs SET active_epoch=NEW.usage_active_epoch,
                      build_epoch=NEW.usage_build_epoch,
                      active_parser_version=NEW.usage_parser_version,
                      build_parser_version=NEW.usage_build_parser_version
                    WHERE source='codex';
                 END;
                 ALTER TABLE threads ADD COLUMN source TEXT NOT NULL DEFAULT 'codex';
                 ALTER TABLE threads ADD COLUMN native_session_id TEXT NOT NULL DEFAULT '';",
            )
            .unwrap();
    connection
}

fn thread(connection: &Connection, id: &str) {
    connection
            .execute(
                "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,
                archived,metadata_quality_status,metadata_resolved_at_ms)
             VALUES (?1,'codex',?1,NULL,?1,'main',0,'complete',1)",
                [id],
            )
            .unwrap();
}

fn source(connection: &Connection, id: i64, thread_id: &str, size: i64, status: &str) {
    connection.execute(
            "INSERT INTO codex_source_files(source_file_id,thread_id,current_path,source_area,
                device_id,inode,file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms)
             VALUES (?1,?2,?3,'sessions',?1,?1,1,?4,1,?5,1)",
            params![id,thread_id,format!("/sessions/{id}.jsonl"),size,status],
        ).unwrap();
}

fn active_state(connection: &Connection, source_id: i64, size: i64) {
    active_state_with_versions(connection, source_id, size, 2, 2);
}

fn active_state_with_versions(
    connection: &Connection,
    source_id: i64,
    size: i64,
    parser_version: i64,
    canonical_algorithm_version: i64,
) {
    connection
        .execute(
            "INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,
                committed_offset,guard_hash,processing_status)
                VALUES (?1,'usage',?2,?3,X'01','ready')",
            params![source_id, parser_version, size],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO codex_usage_source_states(ledger_epoch,source_file_id,file_generation,
                device_id,inode,usage_parser_version,canonical_algorithm_version,
                resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,
                owning_thread_id,root_session_id,continuation_state,chain_state,
                chain_block_reason,updated_at_ms)
             VALUES (1,?1,1,?1,?1,?2,?3,?4,?4,'none',NULL,'root','root',
                     'owning_live','continuous',NULL,1)",
            params![source_id, parser_version, canonical_algorithm_version, size],
        )
        .unwrap();
}

fn visible_usage_event(connection: &Connection, epoch: i64, model: &str) {
    connection
        .execute(
            "INSERT INTO usage_events(
                    ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                    turn_key,model,input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                    reasoning_tokens,total_tokens,quality_status,source_file_id,file_generation,
                    source_start_offset,source_end_offset,created_at_ms,reasoning_effort,
                    estimated_cost_nanos_usd,source,source_epoch)
                 VALUES (?1,'visible-event','normal',10,'root','root','turn-1',?2,
                         10,1,0,5,1,15,'complete',1,1,0,10,10,'medium',100,'codex',?1)",
            params![epoch, model],
        )
        .unwrap();
}

fn prepare_visible_rebuild(connection: &mut Connection, build_model: &str) {
    thread(connection, "root");
    source(connection, 1, "root", 100, "present");
    let parser = crate::codex::normalization::USAGE_PARSER_VERSION;
    let canonical = crate::codex::normalization::canonical_algorithm_for(parser).unwrap();
    connection
        .execute(
            "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1 WHERE id=1",
            [parser],
        )
        .unwrap();
    active_state_with_versions(connection, 1, 100, parser, canonical);
    visible_usage_event(connection, 1, "gpt-visible");

    let snapshot = RebuildLedger::new(connection)
        .begin_or_resume(parser, &[1], 1)
        .unwrap();
    assert_eq!((snapshot.active_epoch, snapshot.build_epoch), (1, 2));
    assert_eq!(
        RebuildLedger::new(connection)
            .record_progress(progress(1, 0, 100, 100, TailProof::None))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    visible_usage_event(connection, 2, build_model);
}

fn progress(source: i64, start: u64, end: u64, size: u64, tail: TailProof) -> SourceProgress {
    SourceProgress {
        source_file_id: source,
        expected_generation: 1,
        start_offset: start,
        last_complete_offset: end,
        observed_raw_size: size,
        expected_guard_hash: (start > 0).then(|| vec![1]),
        guard_hash: (end > 0).then(|| vec![1]),
        tail,
        updated_at_ms: 10,
    }
}

#[test]
fn fresh_build_freezes_active_and_present_members_without_touching_old_epoch() {
    let mut connection = database();
    thread(&connection, "root");
    source(&connection, 1, "root", 100, "missing");
    source(&connection, 2, "root", 80, "present");
    connection
        .execute(
            "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=2 WHERE id=1",
            [],
        )
        .unwrap();
    active_state(&connection, 1, 100);

    let snapshot = RebuildLedger::new(&mut connection)
        .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[2], 10)
        .unwrap();
    assert_eq!((snapshot.active_epoch, snapshot.build_epoch), (1, 2));
    assert_eq!(snapshot.members.len(), 2);
    assert_eq!(snapshot.members[0].membership_reason, "active_contributor");
    assert_eq!(
        snapshot.members[0].completion_status,
        CompletionStatus::Blocked
    );
    assert_eq!(snapshot.members[0].required_through_offset, 100);
    assert_eq!(
        snapshot.members[1].membership_reason,
        "present_at_build_start"
    );
    assert_eq!(
        snapshot.members[1].completion_status,
        CompletionStatus::Pending
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(connection.query_row(
            "SELECT processing_status FROM codex_source_checkpoints WHERE source_file_id=2 AND consumer_kind='usage'",
            [], |row| row.get::<_,String>(0)).unwrap(), "rebuild_required");

    let resumed = RebuildLedger::new(&mut connection)
        .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[2], 11)
        .unwrap();
    assert_eq!(
        resumed, snapshot,
        "restart resumes the frozen manifest idempotently"
    );
    assert!(
        RebuildLedger::new(&mut connection)
            .begin_or_resume(
                crate::codex::normalization::USAGE_PARSER_VERSION + 1,
                &[2],
                12
            )
            .is_err()
    );
}

#[test]
fn bounded_progress_resumes_nonzero_and_activation_waits_for_every_member() {
    let mut connection = database();
    thread(&connection, "root");
    source(&connection, 1, "root", 100, "present");
    source(&connection, 2, "root", 80, "present");
    RebuildLedger::new(&mut connection)
        .begin_or_resume(
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2],
            1,
        )
        .unwrap();
    visible_usage_event(&connection, 1, "gpt-built");
    let ledger = &mut RebuildLedger::new(&mut connection);
    assert_eq!(
        ledger
            .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
            .unwrap(),
        ProgressOutcome::Advanced
    );
    assert_eq!(
        ledger
            .record_progress(progress(1, 50, 100, 100, TailProof::None))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    assert!(ledger.activate(&[1, 2]).is_err());
    let app_before: (i64, Option<i64>) = connection
        .query_row(
            "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(app_before, (0, Some(1)));

    let ledger = &mut RebuildLedger::new(&mut connection);
    ledger.block_source(2, "RETRYABLE", 2).unwrap();
    ledger.retry_blocked(2, 3).unwrap();
    assert_eq!(
        ledger
            .record_progress(progress(
                2,
                0,
                70,
                80,
                TailProof::HalfLine { start_offset: 70 },
            ))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    let activated = ledger.activate(&[1, 2]).unwrap();
    assert_eq!(
        activated,
        ActivationOutcome {
            active_epoch: 1,
            data_revision: 1
        }
    );
    let app: (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        app,
        (1, None, crate::codex::normalization::USAGE_PARSER_VERSION)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM codex_usage_build_sources",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn failed_cas_preserves_old_epoch_and_progress_retry_is_idempotent() {
    let mut connection = database();
    thread(&connection, "root");
    source(&connection, 1, "root", 100, "present");
    source(&connection, 2, "root", 40, "missing");
    RebuildLedger::new(&mut connection)
        .begin_or_resume(crate::codex::normalization::USAGE_PARSER_VERSION, &[1], 1)
        .unwrap();
    let ledger = &mut RebuildLedger::new(&mut connection);
    let bad = progress(1, 10, 100, 100, TailProof::None);
    assert!(ledger.record_progress(bad).is_err());
    assert_eq!(connection.query_row(
            "SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id=1 AND consumer_kind='usage'",
            [], |row| row.get::<_,i64>(0)).unwrap(),0);
    let good = progress(1, 0, 100, 100, TailProof::None);
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(good.clone())
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(good)
            .unwrap(),
        ProgressOutcome::AlreadyApplied
    );
    connection
        .execute(
            "UPDATE codex_source_files SET file_status='present' WHERE source_file_id=2",
            [],
        )
        .unwrap();
    assert!(
        RebuildLedger::new(&mut connection)
            .activate(&[1, 2])
            .is_err(),
        "new present source is not silently omitted"
    );
    let state: (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,(SELECT data_revision FROM app_meta WHERE id=1)
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, (0, Some(1), 0));

    let snapshot = RebuildLedger::new(&mut connection)
        .begin_or_resume(
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2],
            20,
        )
        .unwrap();
    assert_eq!(snapshot.members.len(), 2);
    assert_eq!(
        snapshot.members[0].completion_status,
        CompletionStatus::Rebuilt
    );
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(progress(2, 0, 40, 40, TailProof::None))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    let activated = RebuildLedger::new(&mut connection)
        .activate(&[1, 2])
        .unwrap();
    assert_eq!(activated.active_epoch, 1);
}
#[test]
fn identical_active_and_build_activation_keeps_data_revision_stable() {
    let mut connection = database();
    prepare_visible_rebuild(&mut connection, "gpt-visible");
    let before_revision: i64 = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();

    let activated = RebuildLedger::new(&mut connection).activate(&[1]).unwrap();

    assert_eq!(activated.active_epoch, 2);
    assert_eq!(activated.data_revision, before_revision);
    let state: (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,(SELECT data_revision FROM app_meta WHERE id=1)
                 FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, (2, None, before_revision));
}

#[test]
fn changed_active_and_build_activation_bumps_data_revision_once() {
    let mut connection = database();
    prepare_visible_rebuild(&mut connection, "gpt-changed");
    let before_revision: i64 = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();

    let activated = RebuildLedger::new(&mut connection).activate(&[1]).unwrap();

    assert_eq!(activated.active_epoch, 2);
    assert_eq!(activated.data_revision, before_revision + 1);
    let revision: i64 = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, before_revision + 1);
}

#[test]
fn parser_target_replacement_keeps_manifest_and_resets_every_build_member() {
    let mut connection = database();
    thread(&connection, "root");
    source(&connection, 1, "root", 100, "present");
    source(&connection, 2, "root", 80, "present");

    RebuildLedger::new(&mut connection)
        .begin_or_resume(
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2],
            1,
        )
        .unwrap();
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
            .unwrap(),
        ProgressOutcome::Advanced
    );
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(progress(2, 0, 80, 80, TailProof::None))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );

    // A pre-normalized legacy build target is durable input evidence only;
    // the current parser never starts a new canonical write for parser 2.
    connection
        .execute(
            "UPDATE source_usage_epochs SET build_parser_version=2 WHERE source='codex'",
            [],
        )
        .unwrap();

    let replaced = RebuildLedger::new(&mut connection)
        .begin_or_resume(
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2],
            2,
        )
        .unwrap();
    assert_eq!(replaced.build_epoch, 1);
    assert_eq!(
        replaced.target_parser_version,
        crate::codex::normalization::USAGE_PARSER_VERSION
    );
    assert_eq!(
        replaced
            .members
            .iter()
            .map(|member| member.source_file_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        replaced
            .members
            .iter()
            .all(|member| member.completion_status == CompletionStatus::Pending)
    );

    let checkpoints = connection
        .prepare(
            "SELECT source_file_id,parser_version,committed_offset,processing_status
                 FROM codex_source_checkpoints
                 WHERE consumer_kind='usage' ORDER BY source_file_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        checkpoints,
        vec![
            (
                1,
                crate::codex::normalization::USAGE_PARSER_VERSION,
                0,
                "rebuild_required".into()
            ),
            (
                2,
                crate::codex::normalization::USAGE_PARSER_VERSION,
                0,
                "rebuild_required".into()
            ),
        ]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "parser replacement must discard old-target build-only state"
    );
}

#[test]
fn replacement_preserves_unaffected_build_progress_and_old_manifest_members() {
    let mut connection = database();
    thread(&connection, "root");
    source(&connection, 1, "root", 100, "present");
    source(&connection, 2, "root", 80, "present");
    source(&connection, 3, "root", 60, "present");
    RebuildLedger::new(&mut connection)
        .begin_or_resume(
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2, 3],
            1,
        )
        .unwrap();

    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(progress(1, 0, 50, 100, TailProof::Unverified))
            .unwrap(),
        ProgressOutcome::Advanced
    );
    assert_eq!(
        RebuildLedger::new(&mut connection)
            .record_progress(progress(2, 0, 80, 80, TailProof::None))
            .unwrap(),
        ProgressOutcome::Rebuilt
    );
    // Member 3 becomes missing while still unfinished. It must remain in
    // the replacement membership and stay blocked rather than disappear.
    connection
        .execute(
            "UPDATE codex_source_files SET file_status='missing' WHERE source_file_id=3",
            [],
        )
        .unwrap();
    RebuildLedger::new(&mut connection)
        .block_source(3, "OLD_BUILD_MEMBER_MISSING", 2)
        .unwrap();

    let before2: (String, i64, String, i64, String) = connection
        .query_row(
            "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM codex_usage_build_sources b
                 JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=2",
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
    let before3: (String, i64) = connection
        .query_row(
            "SELECT completion_status,required_through_offset
                 FROM codex_usage_build_sources WHERE build_epoch=1 AND source_file_id=3",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    {
        let tx = connection.transaction().unwrap();
        replace_build_preserving_all_members_tx(
            &tx,
            0,
            1,
            crate::codex::normalization::USAGE_PARSER_VERSION,
            &[1, 2].into_iter().collect(),
            &[1].into_iter().collect(),
            3,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let ids = connection
            .prepare("SELECT source_file_id FROM codex_usage_build_sources WHERE build_epoch=1 ORDER BY source_file_id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
    assert_eq!(ids, vec![1, 2, 3]);

    let affected: (String, i64, String, i64) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,c.committed_offset
                 FROM codex_usage_build_sources b
                 JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(
        affected,
        ("pending".into(), 50, "rebuild_required".into(), 0)
    );

    let after2: (String, i64, String, i64, String) = connection
        .query_row(
            "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM codex_usage_build_sources b
                 JOIN codex_source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=1 AND b.source_file_id=2",
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
    let after3: (String, i64) = connection
        .query_row(
            "SELECT completion_status,required_through_offset
                 FROM codex_usage_build_sources WHERE build_epoch=1 AND source_file_id=3",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        after2, before2,
        "safe rebuilt member progress must be byte-for-byte preserved"
    );
    assert_eq!(
        after3, before3,
        "old missing member must remain blocked and retain proof"
    );
}
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::codex::analytics::skills_usage_snapshot;
use crate::codex::normalization::USAGE_PARSER_VERSION;
use crate::{
    codex::{CodexAdapter, CodexConfig},
    domain::{ScanResult, ScanTrigger},
    ingestion::{IngestionConfig, IngestionCoordinator, RequestDisposition},
    range::ResolvedDay,
    source::SourceRegistry,
    storage::{Ledger, LedgerOptions},
    usage::UsageFilter,
};
use serde_json::{Value, json};

const ROOT: &str = "00000000-03e8-7000-8000-000000000001";
const CHILD: &str = "00000000-07d0-7000-8000-000000000002";
const PRE_TURN: &str = "00000000-0898-7000-8000-000000000003";
const CHILD_TURN: &str = "00000000-0bb8-7000-8000-000000000003";
const ROOT_TURN: &str = "00000000-05dc-7000-8000-000000000004";

static TEMP_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let sequence = TEMP_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "usagi-mu04-b03-{label}-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create fixture root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _root: TempRoot,
    home: PathBuf,
    db: PathBuf,
    main_rollout: PathBuf,
    rollout: PathBuf,
}

struct SkillFixture {
    _root: TempRoot,
    home: PathBuf,
    db: PathBuf,
    rollout: PathBuf,
}

impl SkillFixture {
    fn new(skill_name: &str) -> Self {
        let root = TempRoot::new("s07-skills");
        let home = root.path().join("codex");
        fs::create_dir_all(home.join("sessions")).expect("create sessions directory");
        fs::create_dir_all(home.join("archived_sessions"))
            .expect("create archived sessions directory");
        let rollout = home.join(format!("sessions/rollout-{ROOT}.jsonl"));
        fs::write(&rollout, records_to_bytes(&skill_records(skill_name)))
            .expect("write skill rollout fixture");
        write_skill_state(&home, &rollout);
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            rollout,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(Ledger::open(LedgerOptions::new(&self.db)).expect("open skill fixture"))
    }

    fn scanner(&self, ledger: Arc<Ledger>) -> crate::ingestion::ScanHandle {
        let mut registry = SourceRegistry::new();
        registry
            .register(CodexAdapter::new(CodexConfig::from_home(self.home.clone())))
            .expect("register Codex source");
        IngestionCoordinator::start(IngestionConfig::default(), ledger, registry)
            .expect("start skill scanner")
    }
}

impl Fixture {
    fn new() -> Self {
        let root = TempRoot::new("history");
        let home = root.path().join("codex");
        fs::create_dir_all(home.join("sessions")).expect("create sessions directory");
        fs::create_dir_all(home.join("archived_sessions"))
            .expect("create archived sessions directory");

        let main_rollout = home.join(format!("sessions/rollout-{ROOT}.jsonl"));
        fs::write(&main_rollout, records_to_bytes(&main_records()))
            .expect("write main rollout fixture");
        let records = records();
        let bytes = records_to_bytes(&records);
        let rollout = home.join(format!("sessions/rollout-{CHILD}.jsonl"));
        fs::write(&rollout, &bytes).expect("write rollout fixture");
        write_state(&home, &main_rollout, &rollout);
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            main_rollout,
            rollout,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(Ledger::open(LedgerOptions::new(&self.db)).expect("open fixture ledger"))
    }

    fn scanner(&self, ledger: Arc<Ledger>) -> crate::ingestion::ScanHandle {
        let mut registry = SourceRegistry::new();
        registry
            .register(CodexAdapter::new(CodexConfig::from_home(self.home.clone())))
            .expect("register Codex source");
        IngestionCoordinator::start(IngestionConfig::default(), ledger, registry)
            .expect("start scanner")
    }
}

fn records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": CHILD,
                "cwd": "/work/child",
                "parent_thread_id": ROOT,
                "source": {"subagent": {"other": "guardian"}}
            }
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"type": "turn_started", "turn_id": PRE_TURN}
        }),
        token(5, 5, "2026-08-08T01:00:01Z", "B03_PRE_CONTEXT"),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"type": "turn_complete", "turn_id": PRE_TURN}
        }),
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:02Z",
            "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:03Z",
            "payload": {"turn_id": ROOT_TURN, "model": "replayed-root", "effort": "low"}
        }),
        token(100, 100, "2026-08-08T01:00:04Z", "B03_REPLAY_TOKEN"),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:05Z",
            "payload": {"turn_id": CHILD_TURN, "model": "gpt-5.6-sol", "effort": "medium"}
        }),
        token(12, 7, "2026-08-08T01:00:06Z", "B03_POST_CONTEXT"),
    ]
}

fn skill_records(skill_name: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"id": ROOT, "cwd": "/work/skill", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"turn_id": "s07-skill-turn", "model": "skill-model"}
        }),
        token(10, 10, "2026-08-08T01:00:02Z", "S07_SKILL_TOKEN"),
        json!({
            "type": "response_item",
            "timestamp": "2026-08-08T01:00:03Z",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": json!({
                    "cmd": format!("cat /tmp/.codex/skills/{skill_name}/SKILL.md")
                }).to_string()
            }
        }),
    ]
}

fn main_records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T00:59:00Z",
            "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T00:59:01Z",
            "payload": {"type": "turn_started", "turn_id": PRE_TURN}
        }),
        token(5, 5, "2026-08-08T00:59:02Z", "B03_TRUE_UNRESOLVED"),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T00:59:03Z",
            "payload": {"type": "turn_complete", "turn_id": PRE_TURN}
        }),
    ]
}

fn token(total: i64, last: i64, at: &str, marker: &str) -> Value {
    json!({
        "timestamp": at,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "marker": marker,
            "info": {
                "total_token_usage": {
                    "input_tokens": total,
                    "cached_input_tokens": 0,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                    "total_tokens": total
                },
                "last_token_usage": {
                    "input_tokens": last,
                    "cached_input_tokens": 0,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                    "total_tokens": last
                }
            }
        }
    })
}

fn records_to_bytes(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).expect("serialize fixture record"));
        bytes.push(b'\n');
    }
    bytes
}

fn write_state(home: &Path, main_rollout: &Path, child_rollout: &Path) {
    let connection = Connection::open(home.join("state_5.sqlite")).expect("open state fixture");
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT NOT NULL, rollout_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER,
                archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT, agent_role TEXT
             );
             CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL,
                status TEXT, observed_at_ms INTEGER
             );",
        )
        .expect("create state fixture tables");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,1,2,0,'/work/root','Root',NULL,'root-model','main')",
            params![ROOT, main_rollout.to_str().expect("main rollout path")],
        )
        .expect("insert root state");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,3,4,0,'/work/child','Child',NULL,'child-model','subagent')",
            params![CHILD, child_rollout.to_str().expect("child rollout path")],
        )
        .expect("insert child state");
    connection
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
             VALUES (?1,?2,'spawned',3)",
            params![ROOT, CHILD],
        )
        .expect("insert spawn edge");

    let mut index = Vec::new();
    for id in [ROOT, CHILD] {
        index.extend(
            serde_json::to_vec(&json!({"id": id, "thread_name": format!("name-{id}")}))
                .expect("serialize session index"),
        );
        index.push(b'\n');
    }
    fs::write(home.join("session_index.jsonl"), index).expect("write session index");
}

fn write_skill_state(home: &Path, rollout: &Path) {
    let connection = Connection::open(home.join("state_5.sqlite")).expect("open state fixture");
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT NOT NULL, rollout_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER,
                archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT, agent_role TEXT
             );
             CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL,
                status TEXT, observed_at_ms INTEGER
             );",
        )
        .expect("create skill state tables");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,1,2,0,'/work/skill','Skill',NULL,'skill-model','main')",
            params![ROOT, rollout.to_str().expect("skill rollout path")],
        )
        .expect("insert skill state thread");
    let mut index = serde_json::to_vec(&json!({"id": ROOT, "thread_name": format!("name-{ROOT}")}))
        .expect("serialize skill session index");
    index.push(b'\n');
    fs::write(home.join("session_index.jsonl"), index).expect("write skill session index");
}

fn wait_scan(ledger: &Ledger, wanted: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let scan = ledger.app_state().expect("read scanner state").scan;
        let finished = wanted.is_none_or(|id| scan.last_finished_scan_id.as_deref() == Some(id));
        if finished && scan.active_scan_id.is_none() && scan.last_finished_scan_id.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "scan timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn request_and_wait(handle: &crate::ingestion::ScanHandle, ledger: &Ledger) {
    let scan_id = loop {
        match handle.request(ScanTrigger::Manual) {
            Ok(RequestDisposition::Started { scan_id, .. }) => break scan_id,
            Ok(RequestDisposition::Coalesced {
                followup_scan_id, ..
            }) => break followup_scan_id,
            Err(crate::ingestion::ScanRequestError::Recovering) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("request scanner round failed: {error:?}"),
        }
    };
    wait_scan(ledger, Some(&scan_id));
}

fn source_id(connection: &Connection, thread_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT source_file_id FROM codex_source_files WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .expect("read source id")
}

fn active_events(
    connection: &Connection,
    epoch: i64,
) -> Vec<(i64, i64, String, i64, Option<String>)> {
    let mut statement = connection
        .prepare(
            "SELECT o.source_file_id,o.source_start_offset,e.model,e.total_tokens,e.reasoning_effort
             FROM codex_usage_event_occurrences o
             JOIN usage_events e
               ON e.source='codex' AND e.source_epoch=o.ledger_epoch AND e.event_id=o.event_id
             WHERE o.source='codex' AND o.ledger_epoch=?1
             ORDER BY o.source_file_id,o.source_start_offset",
        )
        .expect("prepare active event query");
    statement
        .query_map([epoch], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query active events")
        .map(|row| row.expect("read active event"))
        .collect()
}

fn seed_parser9_skill_fixture(fixture: &SkillFixture) -> (Arc<Ledger>, i64, i64) {
    let ledger = fixture.ledger();
    let scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial skill scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    scanner.shutdown().expect("stop initial skill scanner");

    let connection = Connection::open(&fixture.db).expect("open seeded skill database");
    let (active_epoch, parser_version): (i64, i64) = connection
        .query_row(
            "SELECT active_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read initial skill epoch");
    assert!(active_epoch > 0);
    assert_eq!(parser_version, USAGE_PARSER_VERSION);
    let source_file_id = source_id(&connection, ROOT);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM codex_skill_usage_events
                 WHERE ledger_epoch=?1 AND source_file_id=?2",
                params![active_epoch, source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count initial skill event"),
        1
    );

    // Fixture seed: this is the parser-9 active state that predates the
    // parser-11 rebuild exercised by the tests below.
    connection
        .execute(
            "UPDATE source_usage_epochs SET active_parser_version=9
             WHERE source='codex'",
            [],
        )
        .expect("seed parser-9 active metadata");
    connection
        .execute(
            "UPDATE codex_source_checkpoints SET parser_version=9
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_file_id],
        )
        .expect("seed parser-9 usage checkpoint");
    connection
        .execute(
            "UPDATE codex_usage_source_states SET usage_parser_version=9
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_epoch, source_file_id],
        )
        .expect("seed parser-9 usage source state");
    drop(connection);
    (ledger, active_epoch, source_file_id)
}

fn all_time_skill_day() -> ResolvedDay {
    ResolvedDay {
        date: "fixture".to_owned(),
        start_ms: 0,
        end_ms: i64::MAX,
    }
}

fn skill_names_for_epoch(connection: &Connection, epoch: i64, source_file_id: i64) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT skill_name FROM codex_skill_usage_events
             WHERE ledger_epoch=?1 AND source_file_id=?2
             ORDER BY skill_name",
        )
        .expect("prepare skill event names");
    statement
        .query_map(params![epoch, source_file_id], |row| row.get(0))
        .expect("query skill event names")
        .map(|row| row.expect("read skill event name"))
        .collect()
}

#[test]
fn t_mu04_b03_parser_v5_shadow_rebuild_repairs_historical_owning_context() {
    let fixture = Fixture::new();
    let ledger = fixture.ledger();
    let first_scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    first_scanner.shutdown().expect("stop initial scanner");

    let connection = Connection::open(&fixture.db).expect("open fixture database");
    let initial_epoch: i64 = connection
        .query_row(
            "SELECT active_epoch FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| row.get(0),
        )
        .expect("read initial epoch");
    assert_eq!(initial_epoch, 1);
    let main_source_id = source_id(&connection, ROOT);
    let child_source_id = source_id(&connection, CHILD);
    let initial_events = active_events(&connection, initial_epoch);
    assert_eq!(
        initial_events.len(),
        2,
        "initial events: {initial_events:?}"
    );
    let main_initial = initial_events
        .iter()
        .find(|row| row.0 == main_source_id)
        .expect("main unresolved event");
    let child_initial = initial_events
        .iter()
        .find(|row| row.0 == child_source_id)
        .expect("child owning event");
    assert_eq!((main_initial.2.as_str(), main_initial.3), ("unknown", 5));
    assert_eq!(
        (
            child_initial.2.as_str(),
            child_initial.3,
            child_initial.4.as_deref()
        ),
        ("gpt-5.6-sol", 7, Some("medium"))
    );
    let raw_before = fs::read(&fixture.rollout).expect("read raw fixture");
    let main_raw_before = fs::read(&fixture.main_rollout).expect("read main fixture");

    // Reconstruct the parser-v4 active epoch that predates the ownership-boundary
    // fix: the genuine pre-context token stays unknown, while the later event
    // has been persisted with the same unknown model and lost effort.
    connection
        .execute(
            "UPDATE source_usage_epochs SET active_parser_version=4
             WHERE source='codex'",
            [],
        )
        .expect("mark active parser v4");
    connection
        .execute(
            "UPDATE codex_source_checkpoints SET parser_version=4
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [child_source_id],
        )
        .expect("mark usage checkpoint parser v4");
    connection
        .execute(
            "UPDATE codex_usage_source_states
             SET usage_parser_version=4,canonical_algorithm_version=4,
                 active_model=NULL,active_model_offset=NULL,
                 active_reasoning_effort=NULL,active_reasoning_effort_offset=NULL
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![initial_epoch, child_source_id],
        )
        .expect("remove historical active context");
    connection
        .execute(
            "UPDATE usage_events SET model='unknown',reasoning_effort=NULL
             WHERE source='codex' AND source_epoch=?1 AND model='gpt-5.6-sol'
               AND event_id IN (
                   SELECT event_id FROM codex_usage_event_occurrences
                   WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               )",
            params![initial_epoch, child_source_id],
        )
        .expect("persist historical unknown event");
    let mut expected_old = vec![
        (
            main_source_id,
            main_initial.1,
            "unknown".to_owned(),
            5,
            None,
        ),
        (
            child_source_id,
            child_initial.1,
            "unknown".to_owned(),
            7,
            None,
        ),
    ];
    expected_old.sort_by_key(|row| row.0);
    assert_eq!(active_events(&connection, initial_epoch), expected_old);
    drop(connection);

    let mut rebuild_connection =
        Connection::open(&fixture.db).expect("open rebuild fixture database");
    let build = RebuildLedger::new(&mut rebuild_connection)
        .begin_or_resume(USAGE_PARSER_VERSION, &[main_source_id, child_source_id], 10)
        .expect("begin parser-v5 shadow rebuild");
    drop(rebuild_connection);
    assert_eq!(build.active_epoch, initial_epoch);
    assert_eq!(build.target_parser_version, USAGE_PARSER_VERSION);
    assert_eq!(build.build_epoch, initial_epoch + 1);
    assert_eq!(build.members.len(), 2);

    // The old active epoch remains the read source until the new build proves
    // every source complete and activation swaps the epoch atomically.
    let connection = Connection::open(&fixture.db).expect("reopen fixture database");
    assert_eq!(active_events(&connection, initial_epoch), expected_old);
    assert_eq!(
        connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get(2)?
                )),
            )
            .expect("read pre-activation epoch"),
        (initial_epoch, Some(build.build_epoch), 4)
    );
    drop(connection);

    let second_scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&second_scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read rebuild scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open rebuilt database");
    let (active_epoch, build_epoch, parser_version): (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read rebuilt epoch");
    assert_eq!(
        active_epoch, build.build_epoch,
        "epoch={active_epoch} build={build_epoch:?} parser={parser_version}"
    );
    assert_eq!(build_epoch, None);
    assert_eq!(parser_version, USAGE_PARSER_VERSION);
    assert_eq!(active_events(&connection, active_epoch).len(), 2);
    assert_eq!(
        active_events(&connection, active_epoch)
            .iter()
            .map(|row| row.3)
            .sum::<i64>(),
        12
    );
    let mut expected_new = vec![
        (main_source_id, "unknown".to_owned(), 5, None),
        (
            child_source_id,
            "gpt-5.6-sol".to_owned(),
            7,
            Some("medium".to_owned()),
        ),
    ];
    expected_new.sort_by_key(|row| row.0);
    assert_eq!(
        active_events(&connection, active_epoch)
            .into_iter()
            .map(|(source, _, model, total, effort)| (source, model, total, effort))
            .collect::<Vec<_>>(),
        expected_new
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1",
                [active_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt occurrences"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(DISTINCT event_id) FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1",
                [active_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt event IDs"),
        2
    );
    let state: (i64, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT usage_parser_version,active_model,active_reasoning_effort
             FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_epoch, child_source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read rebuilt source state");
    assert_eq!(
        state,
        (
            USAGE_PARSER_VERSION,
            Some("gpt-5.6-sol".to_owned()),
            Some("medium".to_owned())
        )
    );
    assert_eq!(
        fs::read(&fixture.rollout).expect("read raw fixture"),
        raw_before
    );
    assert_eq!(
        fs::read(&fixture.main_rollout).expect("read main fixture"),
        main_raw_before
    );
    drop(connection);
    second_scanner.shutdown().expect("stop rebuild scanner");
}

#[test]
fn t_s07_003_rebuild_activation_keeps_parser9_active_until_parser11_completes() {
    assert_eq!(USAGE_PARSER_VERSION, 11);
    let fixture = SkillFixture::new("legacy-skill");
    let (ledger, active_before, source_file_id) = seed_parser9_skill_fixture(&fixture);
    let mut rebuild_connection =
        Connection::open(&fixture.db).expect("open parser-9 rebuild database");
    let build = RebuildLedger::new(&mut rebuild_connection)
        .begin_or_resume(USAGE_PARSER_VERSION, &[source_file_id], 10)
        .expect("begin parser-11 rebuild");
    drop(rebuild_connection);
    assert_eq!(build.active_epoch, active_before);
    assert_eq!(build.target_parser_version, 11);
    assert_eq!(build.build_epoch, active_before + 1);

    let connection = Connection::open(&fixture.db).expect("open parser-9 rebuild database");
    let before: (i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,
                    active_parser_version,build_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read parser-9 rebuild state");
    assert_eq!(
        before,
        (active_before, Some(build.build_epoch), 9, Some(11))
    );
    drop(connection);

    let before_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read parser-9 Skills readiness");
    assert!(!before_snapshot.value.ready);
    assert_eq!(before_snapshot.value.days[0].total, 0);

    let scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read parser-11 rebuild state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open activated parser-11 database");
    let after: (i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,
                    active_parser_version,build_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read activated parser-11 state");
    assert_eq!(after, (build.build_epoch, None, 11, None));
    drop(connection);

    let after_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read parser-11 Skills aggregate");
    assert!(after_snapshot.value.ready);
    assert_eq!(after_snapshot.value.days[0].total, 1);
    assert_eq!(
        after_snapshot.value.days[0].skills[0].skill_name,
        "legacy-skill"
    );
    scanner.shutdown().expect("stop parser-11 scanner");
}

#[test]
fn t_s07_004_skill_event_source_replace_clears_old_rows_before_activation() {
    assert_eq!(USAGE_PARSER_VERSION, 11);
    let fixture = SkillFixture::new("legacy-skill");
    let (ledger, active_before, source_file_id) = seed_parser9_skill_fixture(&fixture);
    let replacement_rollout = fixture.rollout.with_extension("replacement.jsonl");
    fs::write(
        &replacement_rollout,
        records_to_bytes(&skill_records("replacement-skill")),
    )
    .expect("write parser-11 replacement rollout");
    fs::rename(&replacement_rollout, &fixture.rollout)
        .expect("atomically replace parser-11 rollout");

    let mut rebuild_connection =
        Connection::open(&fixture.db).expect("open skill replacement database");
    let build = RebuildLedger::new(&mut rebuild_connection)
        .begin_or_resume(USAGE_PARSER_VERSION, &[source_file_id], 10)
        .expect("begin parser-11 skill replacement");
    drop(rebuild_connection);
    assert_eq!(build.active_epoch, active_before);
    assert_eq!(build.target_parser_version, 11);

    let connection = Connection::open(&fixture.db).expect("open skill replacement database");
    let old_row: (
        i64,
        i64,
        i64,
        i64,
        String,
        String,
        Option<String>,
        String,
        i64,
    ) = connection
        .query_row(
            "SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name,created_at_ms
             FROM codex_skill_usage_events
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_before, source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .expect("read parser-9 skill row");
    assert_eq!(old_row.7, "legacy-skill");
    connection
        .execute(
            "INSERT INTO codex_skill_usage_events(
                ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
                occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                build.build_epoch,
                source_file_id,
                old_row.0,
                old_row.1,
                old_row.2,
                old_row.3,
                old_row.4,
                old_row.5,
                old_row.6,
                old_row.7,
                old_row.8,
            ],
        )
        .expect("seed stale build skill row");
    assert_eq!(
        skill_names_for_epoch(&connection, build.build_epoch, source_file_id),
        vec!["legacy-skill"]
    );
    drop(connection);

    let mut rebuild_connection =
        Connection::open(&fixture.db).expect("open replacement rebuild database");
    RebuildLedger::new(&mut rebuild_connection)
        .replace_build_sources(
            USAGE_PARSER_VERSION,
            &[source_file_id],
            &[source_file_id],
            11,
        )
        .expect("replace parser-11 source build");
    let connection = Connection::open(&fixture.db).expect("reopen replaced skill database");
    assert!(skill_names_for_epoch(&connection, build.build_epoch, source_file_id).is_empty());
    assert_eq!(
        skill_names_for_epoch(&connection, active_before, source_file_id),
        vec!["legacy-skill"]
    );
    drop(connection);

    let before_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read pre-activation Skills readiness");
    assert!(!before_snapshot.value.ready);
    assert_eq!(before_snapshot.value.days[0].total, 0);

    let scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read replacement scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open activated replacement database");
    let (active_epoch, build_epoch, parser_version): (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch,build_epoch,active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read activated replacement state");
    assert_eq!(active_epoch, build.build_epoch);
    assert_eq!(build_epoch, None);
    assert_eq!(parser_version, 11);
    assert_eq!(
        skill_names_for_epoch(&connection, active_epoch, source_file_id),
        vec!["replacement-skill"]
    );
    drop(connection);

    let after_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read activated replacement Skills aggregate");
    assert!(after_snapshot.value.ready);
    assert_eq!(after_snapshot.value.days[0].total, 1);
    assert_eq!(
        after_snapshot.value.days[0].skills[0].skill_name,
        "replacement-skill"
    );
    scanner.shutdown().expect("stop replacement scanner");
}
