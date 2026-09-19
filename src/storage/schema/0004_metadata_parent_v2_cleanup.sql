-- Usagi schema version 4: metadata parent provenance and current-schema cleanup.
--
-- This migration runs inside the migration runner's BEGIN IMMEDIATE transaction.
-- The two target tables are rebuilt so existing facts and all current checks
-- remain intact while the obsolete global app_meta projections are removed.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE rollout_metadata_facts_v4 (
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
        parent_hint_provenance IS NULL OR parent_hint_provenance IN (
            'session_meta_parent',
            'subagent_source',
            'forked_from_id'
        )
    ),
    parent_hint_record_offset INTEGER CHECK (
        parent_hint_record_offset IS NULL OR parent_hint_record_offset >= 0
    ),
    agent_role_hint TEXT,
    agent_role_provenance TEXT CHECK (
        agent_role_provenance IS NULL OR agent_role_provenance IN (
            'session_meta_role',
            'subagent_source'
        )
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
    CHECK (continuation_state <> 'owning_live' OR ownership_confidence = 'confirmed'),
    CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    CHECK (latest_context_at_ms IS NULL OR latest_context_at_ms >= 0),
    CHECK (updated_at_ms >= 0),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id) ON DELETE CASCADE
);

INSERT INTO rollout_metadata_facts_v4 (
    source_file_id, file_generation, metadata_parser_version,
    resolved_through_offset, owning_thread_id, continuation_state,
    cwd, cwd_provenance, cwd_record_offset, created_at_ms,
    latest_context_model, latest_context_at_ms,
    parent_thread_id_hint, parent_hint_provenance, parent_hint_record_offset,
    agent_role_hint, agent_role_provenance, agent_role_record_offset,
    replay_start_offset, owning_records_start_offset,
    ownership_confidence, fact_quality_status, updated_at_ms
)
SELECT
    source_file_id, file_generation, metadata_parser_version,
    resolved_through_offset, owning_thread_id, continuation_state,
    cwd, cwd_provenance, cwd_record_offset, created_at_ms,
    latest_context_model, latest_context_at_ms,
    parent_thread_id_hint, parent_hint_provenance, parent_hint_record_offset,
    agent_role_hint, agent_role_provenance, agent_role_record_offset,
    replay_start_offset, owning_records_start_offset,
    ownership_confidence, fact_quality_status, updated_at_ms
FROM rollout_metadata_facts;

DROP TABLE rollout_metadata_facts;
ALTER TABLE rollout_metadata_facts_v4 RENAME TO rollout_metadata_facts;

CREATE INDEX rollout_metadata_facts_thread_idx
    ON rollout_metadata_facts(owning_thread_id);

CREATE TABLE app_meta_v4 (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data_revision INTEGER NOT NULL CHECK (data_revision >= 0),
    status_revision INTEGER NOT NULL CHECK (status_revision >= 0),
    scan_state TEXT NOT NULL CHECK (scan_state IN ('idle', 'running', 'failed')),
    active_scan_id TEXT CHECK (active_scan_id IS NULL OR length(active_scan_id) > 0),
    last_finished_scan_id TEXT CHECK (last_finished_scan_id IS NULL OR length(last_finished_scan_id) > 0),
    last_finished_scan_result TEXT CHECK (
        last_finished_scan_result IS NULL OR last_finished_scan_result IN ('completed', 'failed')
    ),
    last_scan_started_at_ms INTEGER CHECK (last_scan_started_at_ms IS NULL OR last_scan_started_at_ms >= 0),
    last_scan_completed_at_ms INTEGER CHECK (last_scan_completed_at_ms IS NULL OR last_scan_completed_at_ms >= 0),
    last_scan_failed_at_ms INTEGER CHECK (last_scan_failed_at_ms IS NULL OR last_scan_failed_at_ms >= 0),
    last_scan_error_code TEXT,
    followup_scan_id TEXT CHECK (followup_scan_id IS NULL OR length(followup_scan_id) > 0),
    followup_state TEXT CHECK (followup_state IS NULL OR followup_state IN ('queued', 'start_failed')),
    followup_trigger TEXT CHECK (
        followup_trigger IS NULL OR followup_trigger IN ('Startup', 'Scheduled', 'Manual', 'SourceChanged', 'Rebuild')
    ),
    followup_requested_at_ms INTEGER CHECK (followup_requested_at_ms IS NULL OR followup_requested_at_ms >= 0),
    followup_enqueued_status_revision INTEGER CHECK (
        followup_enqueued_status_revision IS NULL OR followup_enqueued_status_revision >= 0
    ),
    followup_error_code TEXT,
    codex_home_fingerprint TEXT,
    source_binding_status TEXT NOT NULL CHECK (
        source_binding_status IN ('unbound', 'ready', 'source_changed')
    ),
    usage_active_epoch INTEGER NOT NULL DEFAULT 0 CHECK (usage_active_epoch >= 0),
    usage_build_epoch INTEGER CHECK (usage_build_epoch >= 1),
    usage_parser_version INTEGER NOT NULL DEFAULT 0 CHECK (usage_parser_version >= 0),
    usage_build_parser_version INTEGER CHECK (usage_build_parser_version >= 0),
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
    ),
    CHECK ((usage_build_epoch IS NULL) = (usage_build_parser_version IS NULL)),
    CHECK (usage_build_epoch IS NULL OR usage_build_epoch = usage_active_epoch + 1)
);

INSERT INTO app_meta_v4 (
    id, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    codex_home_fingerprint, source_binding_status,
    usage_active_epoch, usage_build_epoch, usage_parser_version, usage_build_parser_version
)
SELECT
    id, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    codex_home_fingerprint, source_binding_status,
    usage_active_epoch, usage_build_epoch, usage_parser_version, usage_build_parser_version
FROM app_meta;

DROP TABLE app_meta;
ALTER TABLE app_meta_v4 RENAME TO app_meta;
