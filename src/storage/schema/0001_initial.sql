-- Usagi schema version 1.
-- The migration runner executes this file inside a BEGIN IMMEDIATE transaction.

CREATE TABLE app_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    metadata_parser_version INTEGER NOT NULL CHECK (metadata_parser_version >= 0),
    data_revision INTEGER NOT NULL CHECK (data_revision >= 0),
    status_revision INTEGER NOT NULL CHECK (status_revision >= 0),
    scan_state TEXT NOT NULL CHECK (scan_state IN ('idle', 'running', 'failed')),
    active_scan_id TEXT CHECK (active_scan_id IS NULL OR length(active_scan_id) > 0),
    last_finished_scan_id TEXT CHECK (last_finished_scan_id IS NULL OR length(last_finished_scan_id) > 0),
    last_finished_scan_result TEXT CHECK (
        last_finished_scan_result IS NULL
        OR last_finished_scan_result IN ('completed', 'failed')
    ),
    last_scan_started_at_ms INTEGER CHECK (
        last_scan_started_at_ms IS NULL OR last_scan_started_at_ms >= 0
    ),
    last_scan_completed_at_ms INTEGER CHECK (
        last_scan_completed_at_ms IS NULL OR last_scan_completed_at_ms >= 0
    ),
    last_scan_failed_at_ms INTEGER CHECK (
        last_scan_failed_at_ms IS NULL OR last_scan_failed_at_ms >= 0
    ),
    last_scan_error_code TEXT,
    followup_scan_id TEXT CHECK (followup_scan_id IS NULL OR length(followup_scan_id) > 0),
    followup_state TEXT CHECK (
        followup_state IS NULL OR followup_state IN ('queued', 'start_failed')
    ),
    followup_trigger TEXT CHECK (
        followup_trigger IS NULL
        OR followup_trigger IN ('Startup', 'Scheduled', 'Manual', 'SourceChanged', 'Rebuild')
    ),
    followup_requested_at_ms INTEGER CHECK (
        followup_requested_at_ms IS NULL OR followup_requested_at_ms >= 0
    ),
    followup_enqueued_status_revision INTEGER CHECK (
        followup_enqueued_status_revision IS NULL OR followup_enqueued_status_revision >= 0
    ),
    followup_error_code TEXT,
    last_full_import_completed_at_ms INTEGER CHECK (
        last_full_import_completed_at_ms IS NULL OR last_full_import_completed_at_ms >= 0
    ),
    codex_home_fingerprint TEXT,
    source_binding_status TEXT NOT NULL CHECK (
        source_binding_status IN ('unbound', 'ready', 'source_changed')
    ),
    CHECK ((last_finished_scan_id IS NULL) = (last_finished_scan_result IS NULL)),
    CHECK ((scan_state = 'running') = (active_scan_id IS NOT NULL)),
    CHECK (
        scan_state IN ('idle', 'failed')
        OR scan_state = 'running'
    ),
    CHECK (active_scan_id IS NULL OR followup_scan_id IS NULL OR active_scan_id <> followup_scan_id),
    CHECK (
        (followup_state IS NULL
            AND followup_scan_id IS NULL
            AND followup_trigger IS NULL
            AND followup_requested_at_ms IS NULL
            AND followup_enqueued_status_revision IS NULL
            AND followup_error_code IS NULL)
        OR (followup_state = 'queued'
            AND followup_scan_id IS NOT NULL
            AND followup_trigger IS NOT NULL
            AND followup_requested_at_ms IS NOT NULL
            AND followup_enqueued_status_revision IS NOT NULL
            AND followup_error_code IS NULL)
        OR (followup_state = 'start_failed'
            AND followup_scan_id IS NOT NULL
            AND followup_trigger IS NOT NULL
            AND followup_requested_at_ms IS NOT NULL
            AND followup_enqueued_status_revision IS NOT NULL
            AND followup_error_code IS NOT NULL)
    ),
    CHECK (
        (source_binding_status = 'unbound' AND codex_home_fingerprint IS NULL)
        OR (source_binding_status IN ('ready', 'source_changed') AND codex_home_fingerprint IS NOT NULL)
    )
);

CREATE TABLE scan_runs (
    scan_id TEXT PRIMARY KEY CHECK (length(scan_id) > 0),
    trigger TEXT NOT NULL CHECK (
        trigger IN ('Startup', 'Scheduled', 'Manual', 'SourceChanged', 'Rebuild')
    ),
    request_kind TEXT NOT NULL CHECK (request_kind IN ('direct', 'followup')),
    state TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'completed', 'failed', 'start_failed')
    ),
    requested_at_ms INTEGER NOT NULL CHECK (requested_at_ms >= 0),
    enqueued_status_revision INTEGER CHECK (
        enqueued_status_revision IS NULL OR enqueued_status_revision >= 0
    ),
    started_at_ms INTEGER CHECK (started_at_ms IS NULL OR started_at_ms >= 0),
    started_status_revision INTEGER CHECK (
        started_status_revision IS NULL OR started_status_revision >= 0
    ),
    finished_at_ms INTEGER CHECK (finished_at_ms IS NULL OR finished_at_ms >= 0),
    terminal_status_revision INTEGER CHECK (
        terminal_status_revision IS NULL OR terminal_status_revision >= 0
    ),
    error_code TEXT,
    CHECK (
        (state = 'queued'
            AND request_kind = 'followup'
            AND enqueued_status_revision IS NOT NULL
            AND started_at_ms IS NULL
            AND started_status_revision IS NULL
            AND finished_at_ms IS NULL
            AND terminal_status_revision IS NULL
            AND error_code IS NULL)
        OR (state = 'running'
            AND started_at_ms IS NOT NULL
            AND started_status_revision IS NOT NULL
            AND finished_at_ms IS NULL
            AND terminal_status_revision IS NULL
            AND error_code IS NULL
            AND ((request_kind = 'direct' AND enqueued_status_revision IS NULL)
                OR (request_kind = 'followup' AND enqueued_status_revision IS NOT NULL)))
        OR (state = 'completed'
            AND started_at_ms IS NOT NULL
            AND started_status_revision IS NOT NULL
            AND finished_at_ms IS NOT NULL
            AND terminal_status_revision IS NOT NULL
            AND error_code IS NULL
            AND ((request_kind = 'direct' AND enqueued_status_revision IS NULL)
                OR (request_kind = 'followup' AND enqueued_status_revision IS NOT NULL)))
        OR (state = 'failed'
            AND started_at_ms IS NOT NULL
            AND started_status_revision IS NOT NULL
            AND finished_at_ms IS NOT NULL
            AND terminal_status_revision IS NOT NULL
            AND error_code IS NOT NULL
            AND ((request_kind = 'direct' AND enqueued_status_revision IS NULL)
                OR (request_kind = 'followup' AND enqueued_status_revision IS NOT NULL)))
        OR (state = 'start_failed'
            AND request_kind = 'followup'
            AND enqueued_status_revision IS NOT NULL
            AND started_at_ms IS NULL
            AND started_status_revision IS NULL
            AND finished_at_ms IS NOT NULL
            AND terminal_status_revision IS NOT NULL
            AND error_code IS NOT NULL)
    )
);

CREATE INDEX idx_scan_runs_state ON scan_runs(state);

CREATE TABLE source_files (
    source_file_id INTEGER PRIMARY KEY,
    thread_id TEXT,
    current_path TEXT NOT NULL UNIQUE,
    source_area TEXT NOT NULL CHECK (source_area IN ('sessions', 'archived_sessions')),
    device_id INTEGER NOT NULL CHECK (device_id >= 0),
    inode INTEGER NOT NULL CHECK (inode >= 0),
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    observed_size INTEGER NOT NULL CHECK (observed_size >= 0),
    observed_mtime_ns INTEGER NOT NULL CHECK (observed_mtime_ns >= 0),
    file_status TEXT NOT NULL CHECK (file_status IN ('present', 'missing', 'replaced')),
    last_seen_at_ms INTEGER NOT NULL CHECK (last_seen_at_ms >= 0),
    UNIQUE (device_id, inode, file_generation)
);

CREATE INDEX source_files_thread_idx ON source_files(thread_id);
CREATE INDEX source_files_status_idx ON source_files(file_status);

CREATE TABLE source_checkpoints (
    source_file_id INTEGER NOT NULL CHECK (source_file_id > 0),
    consumer_kind TEXT NOT NULL CHECK (consumer_kind IN ('metadata', 'usage')),
    parser_version INTEGER NOT NULL CHECK (parser_version >= 0),
    committed_offset INTEGER NOT NULL CHECK (committed_offset >= 0),
    guard_hash BLOB,
    processing_status TEXT NOT NULL CHECK (
        processing_status IN ('pending', 'ready', 'rebuild_required', 'error')
    ),
    last_successful_scan_at_ms INTEGER CHECK (
        last_successful_scan_at_ms IS NULL OR last_successful_scan_at_ms >= 0
    ),
    last_error_code TEXT,
    PRIMARY KEY (source_file_id, consumer_kind),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id) ON DELETE CASCADE
);

CREATE INDEX source_checkpoints_status_idx
    ON source_checkpoints(consumer_kind, processing_status);

CREATE TABLE rollout_metadata_facts (
    source_file_id INTEGER PRIMARY KEY,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    metadata_parser_version INTEGER NOT NULL CHECK (metadata_parser_version >= 0),
    resolved_through_offset INTEGER NOT NULL CHECK (resolved_through_offset >= 0),
    owning_thread_id TEXT NOT NULL,
    continuation_state TEXT NOT NULL CHECK (
        continuation_state IN ('owning_live', 'unstable')
    ),
    cwd TEXT,
    cwd_provenance TEXT CHECK (
        cwd_provenance IS NULL OR cwd_provenance IN ('session_meta', 'turn_context')
    ),
    cwd_record_offset INTEGER CHECK (cwd_record_offset IS NULL OR cwd_record_offset >= 0),
    created_at_ms INTEGER,
    latest_context_model TEXT,
    latest_context_at_ms INTEGER,
    parent_thread_id_hint TEXT,
    parent_hint_provenance TEXT CHECK (
        parent_hint_provenance IS NULL OR parent_hint_provenance IN ('subagent_source', 'forked_from_id')
    ),
    parent_hint_record_offset INTEGER CHECK (
        parent_hint_record_offset IS NULL OR parent_hint_record_offset >= 0
    ),
    agent_role_hint TEXT,
    agent_role_provenance TEXT CHECK (
        agent_role_provenance IS NULL OR agent_role_provenance IN ('session_meta_role', 'subagent_source')
    ),
    agent_role_record_offset INTEGER CHECK (
        agent_role_record_offset IS NULL OR agent_role_record_offset >= 0
    ),
    replay_start_offset INTEGER CHECK (replay_start_offset IS NULL OR replay_start_offset >= 0),
    owning_records_start_offset INTEGER CHECK (
        owning_records_start_offset IS NULL OR owning_records_start_offset >= 0
    ),
    ownership_confidence TEXT NOT NULL CHECK (ownership_confidence IN ('confirmed', 'unresolved')),
    fact_quality_status TEXT NOT NULL CHECK (fact_quality_status IN ('complete', 'partial', 'conflict')),
    updated_at_ms INTEGER NOT NULL,
    CHECK (
        (cwd IS NULL AND cwd_provenance IS NULL AND cwd_record_offset IS NULL)
        OR (cwd IS NOT NULL AND cwd_provenance IS NOT NULL AND cwd_record_offset IS NOT NULL)
    ),
    CHECK (
        (parent_thread_id_hint IS NULL AND parent_hint_provenance IS NULL AND parent_hint_record_offset IS NULL)
        OR (parent_thread_id_hint IS NOT NULL AND parent_hint_provenance IS NOT NULL AND parent_hint_record_offset IS NOT NULL)
    ),
    CHECK (
        (agent_role_hint IS NULL AND agent_role_provenance IS NULL AND agent_role_record_offset IS NULL)
        OR (agent_role_hint IS NOT NULL AND agent_role_provenance IS NOT NULL AND agent_role_record_offset IS NOT NULL)
    ),
    CHECK (
        continuation_state <> 'owning_live'
        OR ownership_confidence = 'confirmed'
    ),
    CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    CHECK (latest_context_at_ms IS NULL OR latest_context_at_ms >= 0),
    CHECK (updated_at_ms >= 0),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id) ON DELETE CASCADE
);

CREATE INDEX rollout_metadata_facts_thread_idx
    ON rollout_metadata_facts(owning_thread_id);

CREATE TABLE threads (
    thread_id TEXT PRIMARY KEY CHECK (length(thread_id) > 0),
    parent_thread_id TEXT,
    root_session_id TEXT,
    agent_role TEXT NOT NULL CHECK (agent_role IN ('main', 'subagent', 'unknown')),
    title TEXT,
    project_name TEXT,
    project_path TEXT,
    metadata_model TEXT,
    created_at_ms INTEGER CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    updated_at_ms INTEGER CHECK (updated_at_ms IS NULL OR updated_at_ms >= 0),
    archived INTEGER NOT NULL CHECK (archived IN (0, 1)),
    current_rollout_path TEXT,
    metadata_quality_status TEXT NOT NULL CHECK (
        metadata_quality_status IN ('complete', 'partial', 'conflict')
    ),
    metadata_resolved_at_ms INTEGER NOT NULL CHECK (metadata_resolved_at_ms >= 0),
    CHECK (
        (agent_role = 'main' AND parent_thread_id IS NULL AND root_session_id = thread_id)
        OR (agent_role = 'subagent' AND parent_thread_id IS NOT NULL)
        OR (agent_role = 'unknown' AND root_session_id IS NULL)
    )
);

CREATE INDEX threads_parent_idx ON threads(parent_thread_id);
CREATE INDEX threads_root_idx ON threads(root_session_id);
CREATE INDEX threads_updated_idx ON threads(updated_at_ms);

-- Keep cross-table checkpoint offsets from ever exceeding the observed file size.
-- The storage layer still validates this invariant before advancing a checkpoint;
-- these triggers protect direct SQL callers and migration/recovery paths as well.
CREATE TRIGGER source_checkpoints_offset_insert
BEFORE INSERT ON source_checkpoints
WHEN NEW.committed_offset > (
    SELECT observed_size FROM source_files WHERE source_file_id = NEW.source_file_id
)
BEGIN
    SELECT RAISE(ABORT, 'checkpoint offset exceeds observed source size');
END;

CREATE TRIGGER source_checkpoints_offset_update
BEFORE UPDATE OF committed_offset, source_file_id ON source_checkpoints
WHEN NEW.committed_offset > (
    SELECT observed_size FROM source_files WHERE source_file_id = NEW.source_file_id
)
BEGIN
    SELECT RAISE(ABORT, 'checkpoint offset exceeds observed source size');
END;

-- A terminal scan is historical fact. It must not be moved back into a
-- queued/running state or have its scan identity reused.
CREATE TRIGGER scan_runs_terminal_state_guard
BEFORE UPDATE OF state ON scan_runs
WHEN OLD.state IN ('completed', 'failed', 'start_failed')
 AND NEW.state IN ('queued', 'running')
BEGIN
    SELECT RAISE(ABORT, 'terminal scan cannot be restarted');
END;

INSERT INTO app_meta (
    id,
    metadata_parser_version,
    data_revision,
    status_revision,
    scan_state,
    source_binding_status
) VALUES (1, 0, 0, 0, 'idle', 'unbound');
