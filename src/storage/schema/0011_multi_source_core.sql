-- Usagi schema version 11: source-neutral canonical sessions and usage.
--
-- This migration is executed by the migration runner in one BEGIN IMMEDIATE
-- transaction with foreign-key enforcement disabled.  The runner performs a
-- foreign_key_check before committing and restores the connection setting on
-- both success and failure.

-- Per-source usage epoch state replaces the four global app_meta projections.
CREATE TABLE source_usage_epochs (
    source TEXT PRIMARY KEY CHECK (length(source) > 0),
    active_epoch INTEGER NOT NULL CHECK (active_epoch >= 0),
    build_epoch INTEGER CHECK (build_epoch >= 1),
    active_parser_version INTEGER NOT NULL CHECK (active_parser_version >= 0),
    build_parser_version INTEGER CHECK (build_parser_version >= 0),
    CHECK ((build_epoch IS NULL) = (build_parser_version IS NULL)),
    CHECK (build_epoch IS NULL OR build_epoch = active_epoch + 1)
);

INSERT INTO source_usage_epochs(
    source, active_epoch, build_epoch, active_parser_version, build_parser_version
)
SELECT 'codex', usage_active_epoch, usage_build_epoch,
       usage_parser_version, usage_build_parser_version
FROM app_meta
WHERE id = 1;

-- Sessions are source-neutral.  Existing Codex ids are deliberately retained
-- as both the canonical id and native id; no historical namespace is added.
CREATE TABLE threads_v11 (
    thread_id TEXT PRIMARY KEY CHECK (length(thread_id) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    native_session_id TEXT NOT NULL CHECK (length(native_session_id) > 0),
    parent_thread_id TEXT,
    root_session_id TEXT,
    agent_role TEXT NOT NULL CHECK (agent_role IN ('main', 'subagent', 'unknown')),
    title TEXT,
    project_name TEXT,
    project_path TEXT,
    project_kind TEXT NOT NULL CHECK (
        project_kind IN ('project', 'projectless', 'unknown')
    ),
    metadata_model TEXT,
    created_at_ms INTEGER CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    updated_at_ms INTEGER CHECK (updated_at_ms IS NULL OR updated_at_ms >= 0),
    archived INTEGER NOT NULL CHECK (archived IN (0, 1)),
    metadata_quality_status TEXT NOT NULL CHECK (
        metadata_quality_status IN ('complete', 'partial', 'conflict')
    ),
    metadata_resolved_at_ms INTEGER NOT NULL CHECK (metadata_resolved_at_ms >= 0),
    UNIQUE (source, native_session_id),
    CHECK (
        (agent_role = 'main' AND parent_thread_id IS NULL AND root_session_id = thread_id)
        OR (agent_role = 'subagent' AND parent_thread_id IS NOT NULL)
        OR (agent_role = 'unknown' AND root_session_id IS NULL)
    )
);

INSERT INTO threads_v11(
    thread_id, source, native_session_id, parent_thread_id, root_session_id,
    agent_role, title, project_name, project_path, project_kind, metadata_model,
    created_at_ms, updated_at_ms, archived, metadata_quality_status,
    metadata_resolved_at_ms
)
SELECT thread_id, 'codex', thread_id, parent_thread_id, root_session_id,
       agent_role, title, project_name, project_path, project_kind, metadata_model,
       created_at_ms, updated_at_ms, archived, metadata_quality_status,
       metadata_resolved_at_ms
FROM threads;

DROP TABLE threads;
ALTER TABLE threads_v11 RENAME TO threads;

CREATE INDEX threads_parent_idx ON threads(parent_thread_id);
CREATE INDEX threads_root_idx ON threads(root_session_id);
CREATE INDEX threads_updated_idx ON threads(updated_at_ms);
CREATE INDEX threads_source_updated_idx ON threads(source, updated_at_ms);

-- Before removing canonical provenance, make sure every event has a complete
-- Codex occurrence.  A missing row is unambiguous because all legacy event
-- provenance columns are NOT NULL; an existing conflicting occurrence is
-- rejected by the guard below rather than silently overwritten.
INSERT INTO usage_event_occurrences(
    ledger_epoch, source_file_id, file_generation, source_start_offset,
    source_end_offset, event_id, created_at_ms
)
SELECT e.ledger_epoch, e.source_file_id, e.file_generation,
       e.source_start_offset, e.source_end_offset, e.event_id, e.created_at_ms
FROM usage_events e
WHERE NOT EXISTS (
    SELECT 1
    FROM usage_event_occurrences o
    WHERE o.ledger_epoch = e.ledger_epoch
      AND o.source_file_id = e.source_file_id
      AND o.file_generation = e.file_generation
      AND o.source_start_offset = e.source_start_offset
);

CREATE TABLE _v11_provenance_guard (
    valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO _v11_provenance_guard(valid)
SELECT 0
FROM usage_events e
WHERE NOT EXISTS (
    SELECT 1
    FROM usage_event_occurrences o
    WHERE o.ledger_epoch = e.ledger_epoch
      AND o.source_file_id = e.source_file_id
      AND o.file_generation = e.file_generation
      AND o.source_start_offset = e.source_start_offset
      AND o.source_end_offset = e.source_end_offset
      AND o.event_id = e.event_id
);
DROP TABLE _v11_provenance_guard;

-- Canonical usage no longer embeds a Codex byte-range occurrence.
CREATE TABLE usage_events_v11 (
    source TEXT NOT NULL CHECK (length(source) > 0),
    source_epoch INTEGER NOT NULL CHECK (source_epoch > 0),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('normal', 'recovered', 'turn_compensation')),
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0),
    thread_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    turn_key TEXT,
    model TEXT NOT NULL CHECK (length(model) > 0),
    reasoning_effort TEXT,
    estimated_cost_nanos_usd INTEGER CHECK (
        estimated_cost_nanos_usd IS NULL OR estimated_cost_nanos_usd >= 0
    ),
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    cached_tokens INTEGER NOT NULL CHECK (cached_tokens >= 0),
    cache_write_tokens INTEGER CHECK (cache_write_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    reasoning_tokens INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
    total_tokens INTEGER NOT NULL CHECK (total_tokens >= 0),
    quality_status TEXT NOT NULL CHECK (quality_status IN ('complete', 'partial')),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (source, source_epoch, event_id),
    FOREIGN KEY (source) REFERENCES source_usage_epochs(source),
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id),
    CHECK (cached_tokens <= input_tokens),
    CHECK (cache_write_tokens IS NULL OR cached_tokens + cache_write_tokens <= input_tokens),
    CHECK (reasoning_tokens <= output_tokens),
    CHECK (total_tokens = input_tokens + output_tokens)
);

INSERT INTO usage_events_v11(
    source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id,
    root_session_id, turn_key, model, reasoning_effort, estimated_cost_nanos_usd,
    input_tokens, cached_tokens, cache_write_tokens, output_tokens,
    reasoning_tokens, total_tokens, quality_status, created_at_ms
)
SELECT 'codex', ledger_epoch, event_id, event_kind, occurred_at_ms, thread_id,
       root_session_id, turn_key, model, reasoning_effort,
       estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
       output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
FROM usage_events;

DROP TABLE usage_events;
ALTER TABLE usage_events_v11 RENAME TO usage_events;

-- Occurrences remain Codex-private provenance, now keyed to the source-aware
-- canonical event identity.  The legacy ledger_epoch name is retained here
-- because this table is Codex-specific state.
CREATE TABLE usage_event_occurrences_v11 (
    source TEXT NOT NULL CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (source, ledger_epoch, source_file_id, file_generation, source_start_offset),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    FOREIGN KEY (source, ledger_epoch, event_id)
        REFERENCES usage_events(source, source_epoch, event_id) DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO usage_event_occurrences_v11(
    source, ledger_epoch, source_file_id, file_generation, source_start_offset,
    source_end_offset, event_id, created_at_ms
)
SELECT 'codex', ledger_epoch, source_file_id, file_generation, source_start_offset,
       source_end_offset, event_id, created_at_ms
FROM usage_event_occurrences;

DROP TABLE usage_event_occurrences;
ALTER TABLE usage_event_occurrences_v11 RENAME TO usage_event_occurrences;

-- app_meta keeps all global lifecycle, revision, and Codex binding fields;
-- only the four per-source usage projections move to source_usage_epochs.
CREATE TABLE app_meta_v11 (
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
    cost_algorithm_version INTEGER NOT NULL DEFAULT 0 CHECK (cost_algorithm_version >= 0),
    pricing_catalog_version INTEGER NOT NULL DEFAULT 0 CHECK (pricing_catalog_version >= 0),
    CHECK ((last_finished_scan_id IS NULL) = (last_finished_scan_result IS NULL)),
    CHECK ((scan_state = 'running') = (active_scan_id IS NOT NULL)),
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

INSERT INTO app_meta_v11(
    id, metadata_parser_version, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    last_full_import_completed_at_ms, codex_home_fingerprint, source_binding_status,
    cost_algorithm_version, pricing_catalog_version
)
SELECT
    id, metadata_parser_version, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    last_full_import_completed_at_ms, codex_home_fingerprint, source_binding_status,
    cost_algorithm_version, pricing_catalog_version
FROM app_meta;

DROP TABLE app_meta;
ALTER TABLE app_meta_v11 RENAME TO app_meta;

CREATE TABLE source_scan_runs (
    scan_id TEXT NOT NULL CHECK (length(scan_id) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    state TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'completed', 'skipped', 'failed')
    ),
    started_at_ms INTEGER CHECK (started_at_ms IS NULL OR started_at_ms >= 0),
    finished_at_ms INTEGER CHECK (finished_at_ms IS NULL OR finished_at_ms >= 0),
    error_code TEXT,
    PRIMARY KEY (scan_id, source),
    FOREIGN KEY (scan_id) REFERENCES scan_runs(scan_id) ON DELETE CASCADE,
    CHECK (
        (state = 'queued'
            AND started_at_ms IS NULL
            AND finished_at_ms IS NULL
            AND error_code IS NULL)
        OR (state = 'running'
            AND started_at_ms IS NOT NULL
            AND finished_at_ms IS NULL
            AND error_code IS NULL)
        OR (state = 'completed'
            AND started_at_ms IS NOT NULL
            AND finished_at_ms IS NOT NULL
            AND error_code IS NULL)
        OR (state = 'skipped'
            AND started_at_ms IS NULL
            AND finished_at_ms IS NOT NULL
            AND error_code IS NULL)
        OR (state = 'failed'
            AND finished_at_ms IS NOT NULL
            AND error_code IS NOT NULL
            AND length(error_code) > 0)
    ),
    CHECK (
        started_at_ms IS NULL
        OR finished_at_ms IS NULL
        OR finished_at_ms >= started_at_ms
    )
);

CREATE INDEX usage_events_time_idx
    ON usage_events(source, source_epoch, occurred_at_ms);
CREATE INDEX usage_events_thread_time_idx
    ON usage_events(source, source_epoch, thread_id, occurred_at_ms);
CREATE INDEX usage_events_root_time_idx
    ON usage_events(source, source_epoch, root_session_id, occurred_at_ms);
CREATE INDEX usage_events_model_time_idx
    ON usage_events(source, source_epoch, model, occurred_at_ms);
CREATE INDEX usage_events_occurred_time_idx
    ON usage_events(occurred_at_ms);
CREATE INDEX usage_event_occurrences_event_idx
    ON usage_event_occurrences(source, ledger_epoch, event_id);
CREATE INDEX usage_event_occurrences_source_idx
    ON usage_event_occurrences(source, ledger_epoch, source_file_id, file_generation, source_start_offset);
CREATE INDEX source_scan_runs_scan_idx
    ON source_scan_runs(scan_id, source);
