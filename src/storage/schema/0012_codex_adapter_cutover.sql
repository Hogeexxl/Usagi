-- Usagi schema version 12: move Codex physical persistence behind its
-- explicit adapter/storage boundary.  This migration deliberately copies the
-- historical binding values; it never resolves the current machine home.

CREATE TABLE codex_adapter_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    home_fingerprint TEXT,
    binding_status TEXT NOT NULL CHECK (
        binding_status IN ('unbound', 'ready', 'source_changed')
    ),
    CHECK (
        (binding_status = 'unbound' AND home_fingerprint IS NULL)
        OR
        (binding_status IN ('ready', 'source_changed') AND home_fingerprint IS NOT NULL)
    )
);

INSERT INTO codex_adapter_state(id, home_fingerprint, binding_status)
SELECT id, codex_home_fingerprint, source_binding_status
FROM app_meta
WHERE id = 1;

PRAGMA legacy_alter_table=OFF;

ALTER TABLE source_files RENAME TO codex_source_files;
ALTER TABLE source_checkpoints RENAME TO codex_source_checkpoints;
ALTER TABLE rollout_metadata_facts RENAME TO codex_rollout_metadata_facts;
ALTER TABLE usage_event_occurrences RENAME TO codex_usage_event_occurrences;
ALTER TABLE turns RENAME TO codex_turns;
ALTER TABLE ingest_anomalies RENAME TO codex_ingest_anomalies;
ALTER TABLE usage_source_states RENAME TO codex_usage_source_states;
ALTER TABLE usage_build_sources RENAME TO codex_usage_build_sources;
ALTER TABLE usage_session_quarantine RENAME TO codex_usage_session_quarantine;
ALTER TABLE usage_session_quarantine_sources RENAME TO codex_usage_session_quarantine_sources;
ALTER TABLE skill_usage_events RENAME TO codex_skill_usage_events;

DROP INDEX source_files_thread_idx;
DROP INDEX source_files_status_idx;
DROP INDEX source_checkpoints_status_idx;
DROP INDEX rollout_metadata_facts_thread_idx;
DROP INDEX usage_event_occurrences_event_idx;
DROP INDEX usage_event_occurrences_source_idx;
DROP INDEX usage_build_sources_status_idx;
DROP INDEX usage_session_quarantine_epoch_idx;
DROP INDEX usage_session_quarantine_sources_source_idx;
DROP INDEX idx_skill_usage_epoch_time;
DROP INDEX idx_skill_usage_epoch_root_time;
DROP INDEX idx_skill_usage_epoch_model_time;
DROP INDEX idx_skill_usage_epoch_source_start;

CREATE INDEX codex_source_files_thread_idx
    ON codex_source_files(thread_id);
CREATE INDEX codex_source_files_status_idx
    ON codex_source_files(file_status);
CREATE INDEX codex_source_checkpoints_status_idx
    ON codex_source_checkpoints(consumer_kind, processing_status);
CREATE INDEX codex_rollout_metadata_facts_thread_idx
    ON codex_rollout_metadata_facts(owning_thread_id);
CREATE INDEX codex_usage_event_occurrences_event_idx
    ON codex_usage_event_occurrences(source, ledger_epoch, event_id);
CREATE INDEX codex_usage_event_occurrences_source_idx
    ON codex_usage_event_occurrences(
        source, ledger_epoch, source_file_id, file_generation, source_start_offset
    );
CREATE INDEX codex_usage_build_sources_status_idx
    ON codex_usage_build_sources(build_epoch, completion_status);
CREATE INDEX codex_usage_session_quarantine_epoch_idx
    ON codex_usage_session_quarantine(ledger_epoch);
CREATE INDEX codex_usage_session_quarantine_sources_source_idx
    ON codex_usage_session_quarantine_sources(ledger_epoch, source_file_id);
CREATE INDEX codex_skill_usage_epoch_time_idx
    ON codex_skill_usage_events(ledger_epoch, occurred_at_ms);
CREATE INDEX codex_skill_usage_epoch_root_time_idx
    ON codex_skill_usage_events(ledger_epoch, root_session_id, occurred_at_ms);
CREATE INDEX codex_skill_usage_epoch_model_time_idx
    ON codex_skill_usage_events(ledger_epoch, model, occurred_at_ms);
CREATE INDEX codex_skill_usage_epoch_source_start_idx
    ON codex_skill_usage_events(ledger_epoch, source_file_id, source_start_offset);

DROP TRIGGER source_checkpoints_offset_insert;
DROP TRIGGER source_checkpoints_offset_update;

CREATE TRIGGER codex_source_checkpoints_offset_insert
BEFORE INSERT ON codex_source_checkpoints
WHEN NEW.committed_offset > (
    SELECT observed_size
    FROM codex_source_files
    WHERE source_file_id = NEW.source_file_id
)
BEGIN
    SELECT RAISE(ABORT, 'checkpoint offset exceeds observed source size');
END;

CREATE TRIGGER codex_source_checkpoints_offset_update
BEFORE UPDATE OF committed_offset, source_file_id ON codex_source_checkpoints
WHEN NEW.committed_offset > (
    SELECT observed_size
    FROM codex_source_files
    WHERE source_file_id = NEW.source_file_id
)
BEGIN
    SELECT RAISE(ABORT, 'checkpoint offset exceeds observed source size');
END;

CREATE TABLE app_meta_v12 (
    id INTEGER PRIMARY KEY CHECK (id = 1),
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
    )
);

INSERT INTO app_meta_v12(
    id, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    cost_algorithm_version, pricing_catalog_version
)
SELECT
    id, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    cost_algorithm_version, pricing_catalog_version
FROM app_meta;

DROP TABLE app_meta;
ALTER TABLE app_meta_v12 RENAME TO app_meta;

PRAGMA foreign_key_check;
