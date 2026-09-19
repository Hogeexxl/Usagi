-- Usagi schema version 2: Token usage ledger foundations.
-- The migration runner executes this file and PRAGMA user_version in one
-- BEGIN IMMEDIATE transaction.

CREATE TABLE _migration_0002_app_meta_guard (
    valid INTEGER NOT NULL CHECK (valid = 1)
);

INSERT INTO _migration_0002_app_meta_guard(valid)
SELECT CASE WHEN count(*) = 1 AND min(id) = 1 AND max(id) = 1 THEN 1 ELSE 0 END
FROM app_meta;

DROP TABLE _migration_0002_app_meta_guard;

CREATE TABLE app_meta_v2 (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    metadata_parser_version INTEGER NOT NULL CHECK (metadata_parser_version >= 0),
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
    last_full_import_completed_at_ms INTEGER CHECK (
        last_full_import_completed_at_ms IS NULL OR last_full_import_completed_at_ms >= 0
    ),
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
        (followup_state IS NULL AND followup_scan_id IS NULL AND followup_trigger IS NULL
            AND followup_requested_at_ms IS NULL AND followup_enqueued_status_revision IS NULL
            AND followup_error_code IS NULL)
        OR (followup_state = 'queued' AND followup_scan_id IS NOT NULL
            AND followup_trigger IS NOT NULL AND followup_requested_at_ms IS NOT NULL
            AND followup_enqueued_status_revision IS NOT NULL AND followup_error_code IS NULL)
        OR (followup_state = 'start_failed' AND followup_scan_id IS NOT NULL
            AND followup_trigger IS NOT NULL AND followup_requested_at_ms IS NOT NULL
            AND followup_enqueued_status_revision IS NOT NULL AND followup_error_code IS NOT NULL)
    ),
    CHECK (
        (source_binding_status = 'unbound' AND codex_home_fingerprint IS NULL)
        OR (source_binding_status IN ('ready', 'source_changed') AND codex_home_fingerprint IS NOT NULL)
    ),
    CHECK ((usage_build_epoch IS NULL) = (usage_build_parser_version IS NULL)),
    CHECK (usage_build_epoch IS NULL OR usage_build_epoch = usage_active_epoch + 1)
);

INSERT INTO app_meta_v2 (
    id, metadata_parser_version, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    last_full_import_completed_at_ms, codex_home_fingerprint, source_binding_status,
    usage_active_epoch, usage_build_epoch, usage_parser_version, usage_build_parser_version
)
SELECT
    id, metadata_parser_version, data_revision, status_revision, scan_state,
    active_scan_id, last_finished_scan_id, last_finished_scan_result,
    last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms,
    last_scan_error_code, followup_scan_id, followup_state, followup_trigger,
    followup_requested_at_ms, followup_enqueued_status_revision, followup_error_code,
    last_full_import_completed_at_ms, codex_home_fingerprint, source_binding_status,
    0, NULL, 0, NULL
FROM app_meta;

DROP TABLE app_meta;
ALTER TABLE app_meta_v2 RENAME TO app_meta;

CREATE TABLE usage_events (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('normal', 'recovered', 'turn_compensation')),
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0),
    thread_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    turn_key TEXT,
    model TEXT NOT NULL CHECK (length(model) > 0),
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    cached_input_tokens INTEGER NOT NULL CHECK (cached_input_tokens >= 0),
    cache_write_input_tokens INTEGER CHECK (cache_write_input_tokens >= 0),
    cache_write_status TEXT NOT NULL CHECK (
        cache_write_status IN ('known', 'unsupported_zero', 'unknown_missing')
    ),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    reasoning_output_tokens INTEGER NOT NULL CHECK (reasoning_output_tokens >= 0),
    total_tokens INTEGER NOT NULL CHECK (total_tokens >= 0),
    quality_status TEXT NOT NULL CHECK (quality_status IN ('complete', 'partial')),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, event_id),
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    CHECK (cached_input_tokens <= input_tokens),
    CHECK (cache_write_input_tokens IS NULL OR cached_input_tokens + cache_write_input_tokens <= input_tokens),
    CHECK (reasoning_output_tokens <= output_tokens),
    CHECK (total_tokens = input_tokens + output_tokens),
    CHECK (
        (cache_write_status = 'known' AND cache_write_input_tokens IS NOT NULL)
        OR (cache_write_status = 'unsupported_zero' AND cache_write_input_tokens = 0)
        OR (cache_write_status = 'unknown_missing' AND cache_write_input_tokens IS NULL)
    ),
    CHECK (
        (quality_status = 'complete' AND cache_write_status <> 'unknown_missing')
        OR (quality_status = 'partial' AND cache_write_status = 'unknown_missing')
    )
);

CREATE INDEX usage_events_time_idx ON usage_events(ledger_epoch, occurred_at_ms);
CREATE INDEX usage_events_thread_time_idx ON usage_events(ledger_epoch, thread_id, occurred_at_ms);
CREATE INDEX usage_events_root_time_idx ON usage_events(ledger_epoch, root_session_id, occurred_at_ms);
CREATE INDEX usage_events_model_time_idx ON usage_events(ledger_epoch, model, occurred_at_ms);

CREATE TABLE usage_event_occurrences (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, source_file_id, file_generation, source_start_offset),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    FOREIGN KEY (ledger_epoch, event_id)
        REFERENCES usage_events(ledger_epoch, event_id) DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX usage_event_occurrences_event_idx
    ON usage_event_occurrences(ledger_epoch, event_id);
CREATE INDEX usage_event_occurrences_source_idx
    ON usage_event_occurrences(ledger_epoch, source_file_id, file_generation, source_start_offset);

CREATE TABLE turns (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    turn_key TEXT NOT NULL CHECK (length(turn_key) > 0),
    thread_id TEXT NOT NULL,
    raw_turn_id TEXT,
    started_at_ms INTEGER CHECK (started_at_ms >= 0),
    ended_at_ms INTEGER CHECK (ended_at_ms >= 0),
    start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
    end_offset INTEGER CHECK (end_offset > start_offset),
    status TEXT NOT NULL CHECK (status IN ('open', 'completed', 'aborted', 'failed')),
    start_total_input_tokens INTEGER CHECK (start_total_input_tokens >= 0),
    start_total_cached_input_tokens INTEGER CHECK (start_total_cached_input_tokens >= 0),
    start_total_cache_write_input_tokens INTEGER CHECK (start_total_cache_write_input_tokens >= 0),
    start_total_output_tokens INTEGER CHECK (start_total_output_tokens >= 0),
    start_total_reasoning_output_tokens INTEGER CHECK (start_total_reasoning_output_tokens >= 0),
    start_total_reported_total_tokens INTEGER CHECK (start_total_reported_total_tokens >= 0),
    start_total_derived_total_tokens INTEGER CHECK (start_total_derived_total_tokens >= 0),
    start_total_cache_write_status TEXT CHECK (
        start_total_cache_write_status IN ('known', 'unsupported_zero', 'unknown_missing')
    ),
    start_total_fingerprint BLOB,
    last_total_input_tokens INTEGER CHECK (last_total_input_tokens >= 0),
    last_total_cached_input_tokens INTEGER CHECK (last_total_cached_input_tokens >= 0),
    last_total_cache_write_input_tokens INTEGER CHECK (last_total_cache_write_input_tokens >= 0),
    last_total_output_tokens INTEGER CHECK (last_total_output_tokens >= 0),
    last_total_reasoning_output_tokens INTEGER CHECK (last_total_reasoning_output_tokens >= 0),
    last_total_reported_total_tokens INTEGER CHECK (last_total_reported_total_tokens >= 0),
    last_total_derived_total_tokens INTEGER CHECK (last_total_derived_total_tokens >= 0),
    last_total_cache_write_status TEXT CHECK (
        last_total_cache_write_status IN ('known', 'unsupported_zero', 'unknown_missing')
    ),
    last_total_fingerprint BLOB,
    accounted_input_tokens INTEGER NOT NULL CHECK (accounted_input_tokens >= 0),
    accounted_cached_input_tokens INTEGER NOT NULL CHECK (accounted_cached_input_tokens >= 0),
    accounted_cache_write_input_tokens INTEGER CHECK (accounted_cache_write_input_tokens >= 0),
    accounted_output_tokens INTEGER NOT NULL CHECK (accounted_output_tokens >= 0),
    accounted_reasoning_output_tokens INTEGER NOT NULL CHECK (accounted_reasoning_output_tokens >= 0),
    accounted_reported_total_tokens INTEGER NOT NULL CHECK (accounted_reported_total_tokens >= 0),
    accounted_derived_total_tokens INTEGER NOT NULL CHECK (accounted_derived_total_tokens >= 0),
    accounted_cache_write_status TEXT NOT NULL CHECK (
        accounted_cache_write_status IN ('known', 'unsupported_zero', 'unknown_missing')
    ),
    accounted_fingerprint BLOB NOT NULL,
    accounted_candidate_count INTEGER NOT NULL CHECK (accounted_candidate_count >= 0),
    model_state TEXT NOT NULL CHECK (model_state IN ('none', 'single', 'mixed')),
    single_model TEXT,
    unresolved_model_seen INTEGER NOT NULL CHECK (unresolved_model_seen IN (0, 1)),
    compensation_allowed INTEGER NOT NULL CHECK (compensation_allowed IN (0, 1)),
    block_start_missing INTEGER NOT NULL CHECK (block_start_missing IN (0, 1)),
    block_time_missing INTEGER NOT NULL CHECK (block_time_missing IN (0, 1)),
    block_reset INTEGER NOT NULL CHECK (block_reset IN (0, 1)),
    block_ownership_gap INTEGER NOT NULL CHECK (block_ownership_gap IN (0, 1)),
    block_parser_gap INTEGER NOT NULL CHECK (block_parser_gap IN (0, 1)),
    block_required_invalid INTEGER NOT NULL CHECK (block_required_invalid IN (0, 1)),
    block_model_unresolved INTEGER NOT NULL CHECK (block_model_unresolved IN (0, 1)),
    quality_status TEXT NOT NULL CHECK (quality_status IN ('complete', 'partial', 'conflict')),
    state_through_offset INTEGER NOT NULL CHECK (state_through_offset >= start_offset),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, source_file_id, file_generation, turn_key),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    CHECK ((status = 'open' AND end_offset IS NULL) OR (status <> 'open' AND end_offset IS NOT NULL)),
    CHECK ((model_state = 'single') = (single_model IS NOT NULL)),
    CHECK (
        compensation_allowed = CASE WHEN
            block_start_missing = 0 AND block_time_missing = 0 AND block_reset = 0
            AND block_ownership_gap = 0 AND block_parser_gap = 0
            AND block_required_invalid = 0 AND block_model_unresolved = 0
        THEN 1 ELSE 0 END
    ),
    CHECK (
        (accounted_cache_write_status = 'unknown_missing' AND accounted_cache_write_input_tokens IS NULL)
        OR (accounted_cache_write_status = 'unsupported_zero' AND accounted_cache_write_input_tokens = 0)
        OR (accounted_cache_write_status = 'known' AND accounted_cache_write_input_tokens IS NOT NULL)
    ),
    CHECK (accounted_cached_input_tokens <= accounted_input_tokens),
    CHECK (accounted_cache_write_input_tokens IS NULL
        OR accounted_cached_input_tokens + accounted_cache_write_input_tokens <= accounted_input_tokens),
    CHECK (accounted_reasoning_output_tokens <= accounted_output_tokens),
    CHECK (accounted_reported_total_tokens = accounted_input_tokens + accounted_output_tokens),
    CHECK (accounted_derived_total_tokens = accounted_input_tokens + accounted_output_tokens),
    CHECK (
        (start_total_input_tokens IS NULL AND start_total_cached_input_tokens IS NULL
            AND start_total_cache_write_input_tokens IS NULL AND start_total_output_tokens IS NULL
            AND start_total_reasoning_output_tokens IS NULL AND start_total_reported_total_tokens IS NULL
            AND start_total_derived_total_tokens IS NULL AND start_total_cache_write_status IS NULL
            AND start_total_fingerprint IS NULL)
        OR (start_total_input_tokens IS NOT NULL AND start_total_cached_input_tokens IS NOT NULL
            AND start_total_output_tokens IS NOT NULL AND start_total_reasoning_output_tokens IS NOT NULL
            AND start_total_reported_total_tokens IS NOT NULL AND start_total_derived_total_tokens IS NOT NULL
            AND start_total_cache_write_status IS NOT NULL AND start_total_fingerprint IS NOT NULL
            AND start_total_cached_input_tokens <= start_total_input_tokens
            AND start_total_reasoning_output_tokens <= start_total_output_tokens
            AND start_total_reported_total_tokens = start_total_input_tokens + start_total_output_tokens
            AND start_total_derived_total_tokens = start_total_input_tokens + start_total_output_tokens
            AND (start_total_cache_write_input_tokens IS NULL
                OR start_total_cached_input_tokens + start_total_cache_write_input_tokens <= start_total_input_tokens)
            AND ((start_total_cache_write_status = 'unknown_missing' AND start_total_cache_write_input_tokens IS NULL)
                OR (start_total_cache_write_status = 'unsupported_zero' AND start_total_cache_write_input_tokens = 0)
                OR (start_total_cache_write_status = 'known' AND start_total_cache_write_input_tokens IS NOT NULL)))
    ),
    CHECK (
        (last_total_input_tokens IS NULL AND last_total_cached_input_tokens IS NULL
            AND last_total_cache_write_input_tokens IS NULL AND last_total_output_tokens IS NULL
            AND last_total_reasoning_output_tokens IS NULL AND last_total_reported_total_tokens IS NULL
            AND last_total_derived_total_tokens IS NULL AND last_total_cache_write_status IS NULL
            AND last_total_fingerprint IS NULL)
        OR (last_total_input_tokens IS NOT NULL AND last_total_cached_input_tokens IS NOT NULL
            AND last_total_output_tokens IS NOT NULL AND last_total_reasoning_output_tokens IS NOT NULL
            AND last_total_reported_total_tokens IS NOT NULL AND last_total_derived_total_tokens IS NOT NULL
            AND last_total_cache_write_status IS NOT NULL AND last_total_fingerprint IS NOT NULL
            AND last_total_cached_input_tokens <= last_total_input_tokens
            AND last_total_reasoning_output_tokens <= last_total_output_tokens
            AND last_total_reported_total_tokens = last_total_input_tokens + last_total_output_tokens
            AND last_total_derived_total_tokens = last_total_input_tokens + last_total_output_tokens
            AND (last_total_cache_write_input_tokens IS NULL
                OR last_total_cached_input_tokens + last_total_cache_write_input_tokens <= last_total_input_tokens)
            AND ((last_total_cache_write_status = 'unknown_missing' AND last_total_cache_write_input_tokens IS NULL)
                OR (last_total_cache_write_status = 'unsupported_zero' AND last_total_cache_write_input_tokens = 0)
                OR (last_total_cache_write_status = 'known' AND last_total_cache_write_input_tokens IS NOT NULL)))
    )
);

CREATE TABLE ingest_anomalies (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    anomaly_id TEXT NOT NULL CHECK (length(anomaly_id) > 0),
    detected_at_ms INTEGER NOT NULL CHECK (detected_at_ms >= 0),
    occurred_at_ms INTEGER CHECK (occurred_at_ms >= 0),
    thread_id TEXT,
    source_file_id INTEGER,
    file_generation INTEGER CHECK (file_generation > 0),
    source_start_offset INTEGER CHECK (source_start_offset >= 0),
    anomaly_type TEXT NOT NULL CHECK (length(anomaly_type) > 0),
    severity TEXT NOT NULL CHECK (severity IN ('warning', 'error')),
    details_json TEXT NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
    PRIMARY KEY (ledger_epoch, anomaly_id),
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    CHECK ((source_file_id IS NULL) = (file_generation IS NULL)),
    CHECK (source_start_offset IS NULL OR source_file_id IS NOT NULL)
);

CREATE TABLE usage_source_states (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    device_id INTEGER NOT NULL CHECK (device_id >= 0),
    inode INTEGER NOT NULL CHECK (inode >= 0),
    usage_parser_version INTEGER NOT NULL CHECK (usage_parser_version >= 0),
    canonical_algorithm_version INTEGER NOT NULL CHECK (canonical_algorithm_version >= 0),
    resolved_through_offset INTEGER NOT NULL CHECK (resolved_through_offset >= 0),
    observed_raw_size INTEGER NOT NULL CHECK (observed_raw_size >= 0),
    raw_tail_status TEXT NOT NULL CHECK (raw_tail_status IN ('unverified', 'none', 'half_line')),
    raw_tail_start_offset INTEGER CHECK (raw_tail_start_offset >= 0),
    owning_thread_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    continuation_state TEXT NOT NULL CHECK (continuation_state = 'owning_live'),
    previous_total_input_tokens INTEGER CHECK (previous_total_input_tokens >= 0),
    previous_total_cached_input_tokens INTEGER CHECK (previous_total_cached_input_tokens >= 0),
    previous_total_cache_write_input_tokens INTEGER CHECK (previous_total_cache_write_input_tokens >= 0),
    previous_total_output_tokens INTEGER CHECK (previous_total_output_tokens >= 0),
    previous_total_reasoning_output_tokens INTEGER CHECK (previous_total_reasoning_output_tokens >= 0),
    previous_total_reported_total_tokens INTEGER CHECK (previous_total_reported_total_tokens >= 0),
    previous_total_derived_total_tokens INTEGER CHECK (previous_total_derived_total_tokens >= 0),
    previous_total_cache_write_status TEXT CHECK (
        previous_total_cache_write_status IN ('known', 'unsupported_zero', 'unknown_missing')
    ),
    previous_total_fingerprint BLOB,
    previous_total_offset INTEGER CHECK (previous_total_offset >= 0),
    chain_state TEXT NOT NULL CHECK (chain_state IN ('continuous', 'interrupted')),
    chain_block_reason TEXT CHECK (
        chain_block_reason IS NULL OR chain_block_reason IN
            ('malformed', 'oversized', 'total_invalid', 'ownership_gap', 'parser_gap')
    ),
    active_turn_key TEXT,
    active_model TEXT,
    active_model_offset INTEGER CHECK (active_model_offset >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    FOREIGN KEY (owning_thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id),
    CHECK (resolved_through_offset <= observed_raw_size),
    CHECK (
        (raw_tail_status = 'unverified' AND raw_tail_start_offset IS NULL)
        OR (raw_tail_status = 'none' AND raw_tail_start_offset IS NULL
            AND resolved_through_offset = observed_raw_size)
        OR (raw_tail_status = 'half_line' AND raw_tail_start_offset = resolved_through_offset
            AND resolved_through_offset < observed_raw_size)
    ),
    CHECK (
        (chain_state = 'continuous' AND chain_block_reason IS NULL)
        OR (chain_state = 'interrupted' AND chain_block_reason IS NOT NULL)
    ),
    CHECK ((active_model IS NULL) = (active_model_offset IS NULL)),
    CHECK (
        (previous_total_input_tokens IS NULL AND previous_total_cached_input_tokens IS NULL
            AND previous_total_cache_write_input_tokens IS NULL AND previous_total_output_tokens IS NULL
            AND previous_total_reasoning_output_tokens IS NULL
            AND previous_total_reported_total_tokens IS NULL
            AND previous_total_derived_total_tokens IS NULL
            AND previous_total_cache_write_status IS NULL AND previous_total_fingerprint IS NULL
            AND previous_total_offset IS NULL)
        OR (previous_total_input_tokens IS NOT NULL AND previous_total_cached_input_tokens IS NOT NULL
            AND previous_total_output_tokens IS NOT NULL
            AND previous_total_reasoning_output_tokens IS NOT NULL
            AND previous_total_reported_total_tokens IS NOT NULL
            AND previous_total_derived_total_tokens IS NOT NULL
            AND previous_total_cache_write_status IS NOT NULL
            AND previous_total_fingerprint IS NOT NULL AND previous_total_offset IS NOT NULL
            AND previous_total_offset <= resolved_through_offset
            AND previous_total_cached_input_tokens <= previous_total_input_tokens
            AND previous_total_reasoning_output_tokens <= previous_total_output_tokens
            AND previous_total_reported_total_tokens = previous_total_input_tokens + previous_total_output_tokens
            AND previous_total_derived_total_tokens = previous_total_input_tokens + previous_total_output_tokens
            AND (previous_total_cache_write_input_tokens IS NULL
                OR previous_total_cached_input_tokens + previous_total_cache_write_input_tokens
                    <= previous_total_input_tokens)
            AND ((previous_total_cache_write_status = 'unknown_missing'
                    AND previous_total_cache_write_input_tokens IS NULL)
                OR (previous_total_cache_write_status = 'unsupported_zero'
                    AND previous_total_cache_write_input_tokens = 0)
                OR (previous_total_cache_write_status = 'known'
                    AND previous_total_cache_write_input_tokens IS NOT NULL)))
    )
);

CREATE TABLE usage_build_sources (
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
    raw_tail_status TEXT NOT NULL CHECK (raw_tail_status IN ('unverified', 'none', 'half_line')),
    raw_tail_start_offset INTEGER CHECK (raw_tail_start_offset >= 0),
    membership_reason TEXT NOT NULL CHECK (
        membership_reason IN ('active_contributor', 'present_at_build_start', 'both', 'discovered_during_build')
    ),
    completion_status TEXT NOT NULL CHECK (completion_status IN ('pending', 'rebuilt', 'carried', 'blocked')),
    completion_error_code TEXT,
    completed_generation INTEGER CHECK (completed_generation > 0),
    completed_through_offset INTEGER CHECK (completed_through_offset >= 0),
    carry_from_epoch INTEGER CHECK (carry_from_epoch >= 0),
    carry_phase TEXT NOT NULL CHECK (carry_phase IN ('none', 'occurrences', 'turns', 'anomalies', 'finalize')),
    carry_after_start_offset INTEGER CHECK (carry_after_start_offset >= 0),
    carry_after_turn_key TEXT,
    carry_after_anomaly_id TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (build_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    CHECK (required_generation = expected_file_generation),
    CHECK (required_through_offset <= observed_raw_size),
    CHECK (
        (raw_tail_status = 'unverified' AND raw_tail_start_offset IS NULL)
        OR (raw_tail_status = 'none' AND raw_tail_start_offset IS NULL
            AND required_through_offset = observed_raw_size)
        OR (raw_tail_status = 'half_line' AND raw_tail_start_offset = required_through_offset
            AND required_through_offset < observed_raw_size)
    ),
    CHECK (
        (completion_status IN ('pending', 'blocked')
            AND completed_generation IS NULL AND completed_through_offset IS NULL)
        OR (completion_status IN ('rebuilt', 'carried')
            AND completed_generation = required_generation
            AND completed_through_offset IS NOT NULL
            AND completed_through_offset >= required_through_offset)
    ),
    CHECK ((completion_status = 'blocked') = (completion_error_code IS NOT NULL)),
    CHECK (
        (carry_phase = 'none' AND carry_from_epoch IS NULL
            AND carry_after_start_offset IS NULL AND carry_after_turn_key IS NULL
            AND carry_after_anomaly_id IS NULL)
        OR (carry_phase <> 'none' AND carry_from_epoch IS NOT NULL)
    )
);

CREATE INDEX usage_build_sources_status_idx
    ON usage_build_sources(build_epoch, completion_status);
