ALTER TABLE codex_usage_source_states ADD COLUMN reconciliation_state_json TEXT NOT NULL
    DEFAULT '{"version":1,"open_window_start_offset":null,"pending_response_ids":[],"modern_counter_domain":null,"modern_counter_total":null,"pending_evidence":[]}'
    CHECK (json_valid(reconciliation_state_json));

CREATE TABLE codex_usage_event_facts (
    source TEXT NOT NULL DEFAULT 'codex' CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    owning_thread_id TEXT NOT NULL,
    response_id TEXT CHECK (response_id IS NULL OR length(response_id) > 0),
    evidence_kind TEXT NOT NULL CHECK (evidence_kind IN ('explicit', 'legacy')),
    operation TEXT NOT NULL CHECK (operation IN ('response', 'compaction')),
    PRIMARY KEY (source, ledger_epoch, event_id),
    FOREIGN KEY (owning_thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (source, ledger_epoch, event_id)
        REFERENCES usage_events(source, source_epoch, event_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    CHECK ((evidence_kind = 'explicit' AND response_id IS NOT NULL)
        OR (evidence_kind = 'legacy' AND response_id IS NULL)),
    CHECK (operation <> 'compaction' OR evidence_kind = 'explicit')
);
CREATE UNIQUE INDEX codex_usage_response_identity_idx
    ON codex_usage_event_facts(source, ledger_epoch, owning_thread_id, response_id)
    WHERE response_id IS NOT NULL;

CREATE TABLE codex_compaction_markers (
    source TEXT NOT NULL DEFAULT 'codex' CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    owning_thread_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    occurred_at_ms INTEGER CHECK (occurred_at_ms IS NULL OR occurred_at_ms >= 0),
    model TEXT CHECK (model IS NULL OR length(model) > 0),
    reasoning_effort TEXT,
    response_id TEXT CHECK (response_id IS NULL OR length(response_id) > 0),
    resolved_event_id TEXT CHECK (resolved_event_id IS NULL OR length(resolved_event_id) > 0),
    unknown_reason TEXT CHECK (unknown_reason IN
        ('usage_missing', 'identity_missing', 'usage_invalid', 'time_missing', 'model_unresolved')),
    PRIMARY KEY (source, ledger_epoch, source_file_id, file_generation, source_start_offset),
    FOREIGN KEY (source) REFERENCES source_usage_epochs(source),
    FOREIGN KEY (source_file_id) REFERENCES codex_source_files(source_file_id) ON DELETE CASCADE,
    FOREIGN KEY (owning_thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id),
    FOREIGN KEY (source, ledger_epoch, resolved_event_id)
        REFERENCES usage_events(source, source_epoch, event_id)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK ((resolved_event_id IS NULL AND unknown_reason IS NOT NULL)
        OR (resolved_event_id IS NOT NULL AND unknown_reason IS NULL)),
    CHECK (resolved_event_id IS NULL OR response_id IS NOT NULL)
);
CREATE INDEX codex_compaction_marker_scope_idx
    ON codex_compaction_markers(ledger_epoch, root_session_id, owning_thread_id, occurred_at_ms);
CREATE INDEX codex_compaction_marker_response_idx
    ON codex_compaction_markers(ledger_epoch, owning_thread_id, response_id);

CREATE TABLE codex_usage_reconciliation_windows (
    source TEXT NOT NULL DEFAULT 'codex' CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    owning_thread_id TEXT NOT NULL,
    turn_key TEXT,
    state_json TEXT NOT NULL CHECK (json_valid(state_json)),
    PRIMARY KEY (source, ledger_epoch, source_file_id, file_generation, source_start_offset),
    FOREIGN KEY (source) REFERENCES source_usage_epochs(source),
    FOREIGN KEY (source_file_id) REFERENCES codex_source_files(source_file_id) ON DELETE CASCADE,
    FOREIGN KEY (owning_thread_id) REFERENCES threads(thread_id)
);
CREATE INDEX codex_usage_window_thread_idx
    ON codex_usage_reconciliation_windows(ledger_epoch, owning_thread_id, turn_key);

CREATE TABLE codex_usage_event_holds (
    source TEXT NOT NULL DEFAULT 'codex' CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    hold_reason TEXT NOT NULL CHECK (hold_reason IN ('replay', 'carry')),
    PRIMARY KEY (source, ledger_epoch, source_file_id, file_generation, event_id),
    FOREIGN KEY (source_file_id) REFERENCES codex_source_files(source_file_id) ON DELETE CASCADE,
    FOREIGN KEY (source, ledger_epoch, event_id)
        REFERENCES usage_events(source, source_epoch, event_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX codex_usage_event_hold_event_idx
    ON codex_usage_event_holds(source, ledger_epoch, event_id);

CREATE TRIGGER codex_usage_fact_owner_insert
BEFORE INSERT ON codex_usage_event_facts
WHEN NOT EXISTS (
    SELECT 1 FROM usage_events e
    WHERE e.source = NEW.source AND e.source_epoch = NEW.ledger_epoch
      AND e.event_id = NEW.event_id AND e.thread_id = NEW.owning_thread_id
)
BEGIN
    SELECT RAISE(ABORT, 'codex usage fact ownership mismatch');
END;
CREATE TRIGGER codex_usage_fact_owner_update
BEFORE UPDATE OF owning_thread_id,source,ledger_epoch,event_id ON codex_usage_event_facts
WHEN NOT EXISTS (
    SELECT 1 FROM usage_events e
    WHERE e.source = NEW.source AND e.source_epoch = NEW.ledger_epoch
      AND e.event_id = NEW.event_id AND e.thread_id = NEW.owning_thread_id
)
BEGIN
    SELECT RAISE(ABORT, 'codex usage fact ownership mismatch');
END;
CREATE TRIGGER codex_compaction_marker_binding_insert
BEFORE INSERT ON codex_compaction_markers
WHEN NEW.resolved_event_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM codex_usage_event_facts f JOIN usage_events e
      ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
    WHERE f.source=NEW.source AND f.ledger_epoch=NEW.ledger_epoch
      AND f.event_id=NEW.resolved_event_id AND f.owning_thread_id=NEW.owning_thread_id
      AND f.response_id=NEW.response_id AND f.operation='compaction'
      AND e.root_session_id=NEW.root_session_id
)
BEGIN
    SELECT RAISE(ABORT, 'codex compaction marker binding mismatch');
END;
CREATE TRIGGER codex_compaction_marker_binding_update
BEFORE UPDATE OF source,ledger_epoch,resolved_event_id,owning_thread_id,response_id,root_session_id
ON codex_compaction_markers
WHEN NEW.resolved_event_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM codex_usage_event_facts f JOIN usage_events e
      ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
    WHERE f.source=NEW.source AND f.ledger_epoch=NEW.ledger_epoch
      AND f.event_id=NEW.resolved_event_id AND f.owning_thread_id=NEW.owning_thread_id
      AND f.response_id=NEW.response_id AND f.operation='compaction'
      AND e.root_session_id=NEW.root_session_id
)
BEGIN
    SELECT RAISE(ABORT, 'codex compaction marker binding mismatch');
END;
CREATE TRIGGER codex_usage_bound_fact_update
BEFORE UPDATE OF source,ledger_epoch,event_id,owning_thread_id,response_id,operation
ON codex_usage_event_facts
WHEN EXISTS (
    SELECT 1 FROM codex_compaction_markers m
    WHERE m.source=OLD.source AND m.ledger_epoch=OLD.ledger_epoch
      AND m.resolved_event_id=OLD.event_id
      AND (NEW.source<>OLD.source OR NEW.ledger_epoch<>OLD.ledger_epoch
        OR NEW.event_id<>OLD.event_id OR NEW.owning_thread_id<>m.owning_thread_id
        OR NEW.response_id IS NOT m.response_id OR NEW.operation<>'compaction')
)
BEGIN
    SELECT RAISE(ABORT, 'codex bound compaction fact mutation');
END;
CREATE TRIGGER codex_usage_bound_fact_delete
BEFORE DELETE ON codex_usage_event_facts
WHEN EXISTS (
    SELECT 1 FROM codex_compaction_markers m
    WHERE m.source=OLD.source AND m.ledger_epoch=OLD.ledger_epoch
      AND m.resolved_event_id=OLD.event_id
)
BEGIN
    SELECT RAISE(ABORT, 'codex bound compaction fact deletion');
END;

ALTER TABLE codex_usage_build_sources RENAME TO codex_usage_build_sources_v13;
CREATE TABLE "codex_usage_build_sources" (
    build_epoch INTEGER NOT NULL CHECK (build_epoch > 0),
    source_file_id INTEGER NOT NULL,
    target_parser_version INTEGER NOT NULL CHECK (target_parser_version >= 0),
    expected_file_generation INTEGER NOT NULL CHECK (expected_file_generation > 0),
    expected_device_id INTEGER NOT NULL CHECK (expected_device_id >= 0),
    expected_inode INTEGER NOT NULL CHECK (expected_inode >= 0),
    expected_owning_thread_id TEXT,
    expected_root_session_id TEXT,
    active_committed_offset INTEGER NOT NULL CHECK (active_committed_offset >= 0),
    active_guard_hash BLOB,
    active_state_fingerprint BLOB,
    required_generation INTEGER NOT NULL CHECK (required_generation > 0),
    required_through_offset INTEGER NOT NULL CHECK (required_through_offset >= 0),
    observed_raw_size INTEGER NOT NULL CHECK (observed_raw_size >= 0),
    raw_tail_status TEXT NOT NULL CHECK (raw_tail_status IN ('unverified','none','half_line')),
    raw_tail_start_offset INTEGER CHECK (raw_tail_start_offset >= 0),
    membership_reason TEXT NOT NULL CHECK (membership_reason IN ('active_contributor','present_at_build_start','both','discovered_during_build')),
    completion_status TEXT NOT NULL CHECK (completion_status IN ('pending','rebuilt','carried','blocked','quarantined')),
    completion_error_code TEXT,
    completed_generation INTEGER CHECK (completed_generation > 0),
    completed_through_offset INTEGER CHECK (completed_through_offset >= 0),
    carry_from_epoch INTEGER CHECK (carry_from_epoch >= 0),
    carry_phase TEXT NOT NULL CHECK (carry_phase IN ('none','occurrences','facts','markers','windows','turns','anomalies','finalize')),
    carry_after_start_offset INTEGER CHECK (carry_after_start_offset >= 0),
    carry_after_turn_key TEXT,
    carry_after_anomaly_id TEXT,
    carry_after_fact_event_id TEXT CHECK (carry_after_fact_event_id IS NULL OR length(carry_after_fact_event_id)>0),
    carry_after_marker_start_offset INTEGER CHECK (carry_after_marker_start_offset>=0),
    carry_after_window_start_offset INTEGER CHECK (carry_after_window_start_offset>=0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (build_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id),
    CHECK (required_generation = expected_file_generation),
    CHECK (required_through_offset <= observed_raw_size),
    CHECK ((raw_tail_status='unverified' AND raw_tail_start_offset IS NULL) OR (raw_tail_status='none' AND raw_tail_start_offset IS NULL AND required_through_offset=observed_raw_size) OR (raw_tail_status='half_line' AND raw_tail_start_offset=required_through_offset AND required_through_offset<observed_raw_size)),
    CHECK ((completion_status IN ('pending','blocked','quarantined') AND completed_generation IS NULL AND completed_through_offset IS NULL) OR (completion_status IN ('rebuilt','carried') AND completed_generation=required_generation AND completed_through_offset IS NOT NULL AND completed_through_offset>=required_through_offset)),
    CHECK ((completion_status IN ('blocked','quarantined')) = (completion_error_code IS NOT NULL)),
    CHECK (completion_status <> 'quarantined' OR expected_root_session_id IS NOT NULL),
    CHECK ((carry_phase='none' AND carry_from_epoch IS NULL AND carry_after_start_offset IS NULL AND carry_after_turn_key IS NULL AND carry_after_anomaly_id IS NULL AND carry_after_fact_event_id IS NULL AND carry_after_marker_start_offset IS NULL AND carry_after_window_start_offset IS NULL) OR (carry_phase<>'none' AND carry_from_epoch IS NOT NULL))
);
INSERT INTO codex_usage_build_sources(build_epoch,source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,required_generation,required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,membership_reason,completion_status,completion_error_code,completed_generation,completed_through_offset,carry_from_epoch,carry_phase,carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,created_at_ms,updated_at_ms)
SELECT build_epoch,source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,required_generation,required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,membership_reason,completion_status,completion_error_code,completed_generation,completed_through_offset,carry_from_epoch,carry_phase,carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,created_at_ms,updated_at_ms FROM codex_usage_build_sources_v13;
DROP TABLE codex_usage_build_sources_v13;
CREATE INDEX codex_usage_build_sources_status_idx
    ON codex_usage_build_sources(build_epoch, completion_status);
