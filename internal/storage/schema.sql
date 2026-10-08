CREATE TABLE antigravity_conversation_state (
    conversation_id TEXT PRIMARY KEY
        CHECK (length(conversation_id) > 0),
    observed_gen_max_idx INTEGER NOT NULL DEFAULT -1
        CHECK (observed_gen_max_idx >= -1),
    observed_step_max_idx INTEGER NOT NULL DEFAULT -1
        CHECK (observed_step_max_idx >= -1),
    last_scanned_at_ms INTEGER NOT NULL
        CHECK (last_scanned_at_ms >= 0)
);

CREATE TABLE antigravity_usage_quarantine (
    conversation_id TEXT NOT NULL
        CHECK (length(conversation_id) > 0),
    payload_digest TEXT NOT NULL
        CHECK (
            length(payload_digest) = 64
            AND payload_digest NOT GLOB '*[^0-9a-f]*'
        ),
    gen_idx INTEGER
        CHECK (gen_idx IS NULL OR gen_idx >= 0),
    response_id TEXT,
    reason_code TEXT NOT NULL
        CHECK (
            reason_code IN (
                'GEN_METADATA_MALFORMED',
                'USAGE_RESPONSE_ID_MISSING',
                'USAGE_RESPONSE_ID_INVALID',
                'USAGE_RESPONSE_ID_CONFLICT',
                'USAGE_MODEL_MISSING',
                'USAGE_MODEL_INVALID',
                'USAGE_STEP_NOT_FOUND',
                'USAGE_STEP_NOT_UNIQUE',
                'USAGE_STEP_KIND_MISMATCH',
                'USAGE_STEP_METADATA_INVALID',
                'USAGE_TIMESTAMP_INVALID',
                'USAGE_TOKEN_INVALID',
                'USAGE_EVENT_MUTATION_CONFLICT'
            )
        ),
    first_seen_at_ms INTEGER NOT NULL
        CHECK (first_seen_at_ms >= 0),
    last_seen_at_ms INTEGER NOT NULL
        CHECK (last_seen_at_ms >= first_seen_at_ms),
    PRIMARY KEY (conversation_id, payload_digest, reason_code),
    FOREIGN KEY (conversation_id)
        REFERENCES antigravity_conversation_state(conversation_id)
        ON DELETE CASCADE
);

CREATE TABLE "app_meta" (
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

CREATE TABLE "codex_ingest_anomalies" (
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
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id),
    CHECK ((source_file_id IS NULL) = (file_generation IS NULL)),
    CHECK (source_start_offset IS NULL OR source_file_id IS NOT NULL)
);

CREATE TABLE "codex_rollout_metadata_facts" (
    source_file_id INTEGER PRIMARY KEY,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    metadata_parser_version INTEGER NOT NULL CHECK (metadata_parser_version >= 0),
    resolved_through_offset INTEGER NOT NULL CHECK (resolved_through_offset >= 0),
    owning_thread_id TEXT NOT NULL,
    continuation_state TEXT NOT NULL CHECK (
        continuation_state IN ('replayed_ancestor', 'owning_live', 'unstable')
    ),
    cwd TEXT,
    cwd_provenance TEXT CHECK (cwd_provenance IS NULL OR cwd_provenance IN ('session_meta', 'turn_context')),
    cwd_record_offset INTEGER CHECK (cwd_record_offset IS NULL OR cwd_record_offset >= 0),
    created_at_ms INTEGER,
    latest_context_model TEXT,
    latest_context_at_ms INTEGER,
    parent_thread_id_hint TEXT,
    parent_hint_provenance TEXT CHECK (parent_hint_provenance IS NULL OR parent_hint_provenance IN ('session_meta_parent','subagent_source','forked_from_id')),
    parent_hint_record_offset INTEGER CHECK (parent_hint_record_offset IS NULL OR parent_hint_record_offset >= 0),
    agent_role_hint TEXT,
    agent_role_provenance TEXT CHECK (agent_role_provenance IS NULL OR agent_role_provenance IN ('session_meta_role','subagent_source')),
    agent_role_record_offset INTEGER CHECK (agent_role_record_offset IS NULL OR agent_role_record_offset >= 0),
    agent_path TEXT,
    agent_path_provenance TEXT CHECK (agent_path_provenance IS NULL OR agent_path_provenance IN ('session_meta','thread_spawn')),
    agent_path_record_offset INTEGER CHECK (agent_path_record_offset IS NULL OR agent_path_record_offset >= 0),
    replay_start_offset INTEGER CHECK (replay_start_offset IS NULL OR replay_start_offset >= 0),
    owning_records_start_offset INTEGER CHECK (owning_records_start_offset IS NULL OR owning_records_start_offset >= 0),
    ownership_confidence TEXT NOT NULL CHECK (ownership_confidence IN ('confirmed', 'unresolved')),
    fact_quality_status TEXT NOT NULL CHECK (fact_quality_status IN ('complete', 'partial', 'conflict')),
    updated_at_ms INTEGER NOT NULL, latest_context_turn_id TEXT, relationship_conflict INTEGER NOT NULL DEFAULT 0
        CHECK (relationship_conflict IN (0, 1)),
    CHECK ((cwd IS NULL AND cwd_provenance IS NULL AND cwd_record_offset IS NULL) OR (cwd IS NOT NULL AND cwd_provenance IS NOT NULL AND cwd_record_offset IS NOT NULL)),
    CHECK ((parent_thread_id_hint IS NULL AND parent_hint_provenance IS NULL AND parent_hint_record_offset IS NULL) OR (parent_thread_id_hint IS NOT NULL AND parent_hint_provenance IS NOT NULL AND parent_hint_record_offset IS NOT NULL)),
    CHECK ((agent_role_hint IS NULL AND agent_role_provenance IS NULL AND agent_role_record_offset IS NULL) OR (agent_role_hint IS NOT NULL AND agent_role_provenance IS NOT NULL AND agent_role_record_offset IS NOT NULL)),
    CHECK ((agent_path IS NULL AND agent_path_provenance IS NULL AND agent_path_record_offset IS NULL) OR (agent_path IS NOT NULL AND agent_path_provenance IS NOT NULL AND agent_path_record_offset IS NOT NULL)),
    CHECK (continuation_state NOT IN ('replayed_ancestor','owning_live') OR ownership_confidence = 'confirmed'),
    CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    CHECK (latest_context_at_ms IS NULL OR latest_context_at_ms >= 0),
    CHECK (updated_at_ms >= 0),
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id) ON DELETE CASCADE
);

CREATE TABLE "codex_skill_usage_events" (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0),
    thread_id TEXT NOT NULL CHECK (length(thread_id) > 0),
    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
    model TEXT,
    skill_name TEXT NOT NULL CHECK (length(skill_name) > 0 AND length(skill_name) <= 128),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, source_file_id, file_generation, source_start_offset, skill_name),
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id) ON DELETE CASCADE
);

CREATE TABLE "codex_source_checkpoints" (
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
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id) ON DELETE CASCADE
);

CREATE TABLE "codex_source_files" (
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

CREATE TABLE "codex_turns" (
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
    start_total_cached_tokens INTEGER CHECK (start_total_cached_tokens >= 0),
    start_total_cache_write_tokens INTEGER CHECK (start_total_cache_write_tokens >= 0),
    start_total_output_tokens INTEGER CHECK (start_total_output_tokens >= 0),
    start_total_reasoning_tokens INTEGER CHECK (start_total_reasoning_tokens >= 0),
    start_total_total_tokens INTEGER CHECK (start_total_total_tokens >= 0),
    start_total_fingerprint BLOB,
    last_total_input_tokens INTEGER CHECK (last_total_input_tokens >= 0),
    last_total_cached_tokens INTEGER CHECK (last_total_cached_tokens >= 0),
    last_total_cache_write_tokens INTEGER CHECK (last_total_cache_write_tokens >= 0),
    last_total_output_tokens INTEGER CHECK (last_total_output_tokens >= 0),
    last_total_reasoning_tokens INTEGER CHECK (last_total_reasoning_tokens >= 0),
    last_total_total_tokens INTEGER CHECK (last_total_total_tokens >= 0),
    last_total_fingerprint BLOB,
    accounted_input_tokens INTEGER NOT NULL CHECK (accounted_input_tokens >= 0),
    accounted_cached_tokens INTEGER NOT NULL CHECK (accounted_cached_tokens >= 0),
    accounted_cache_write_tokens INTEGER CHECK (accounted_cache_write_tokens >= 0),
    accounted_output_tokens INTEGER NOT NULL CHECK (accounted_output_tokens >= 0),
    accounted_reasoning_tokens INTEGER NOT NULL CHECK (accounted_reasoning_tokens >= 0),
    accounted_total_tokens INTEGER NOT NULL CHECK (accounted_total_tokens >= 0),
    accounted_fingerprint BLOB NOT NULL,
    accounted_candidate_count INTEGER NOT NULL CHECK (accounted_candidate_count >= 0),
    model_state TEXT NOT NULL CHECK (model_state IN ('none', 'single', 'mixed')),
    single_model TEXT,
    unresolved_model_seen INTEGER NOT NULL CHECK (unresolved_model_seen IN (0, 1)),
    reasoning_effort_state TEXT NOT NULL CHECK (reasoning_effort_state IN ('none', 'single', 'mixed')),
    single_reasoning_effort TEXT,
    unresolved_reasoning_effort_seen INTEGER NOT NULL CHECK (unresolved_reasoning_effort_seen IN (0, 1)),
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
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id),
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    CHECK ((status = 'open' AND end_offset IS NULL) OR (status <> 'open' AND end_offset IS NOT NULL)),
    CHECK ((model_state = 'single') = (single_model IS NOT NULL)),
    CHECK ((reasoning_effort_state = 'single') = (single_reasoning_effort IS NOT NULL)),
    CHECK (compensation_allowed = CASE WHEN block_start_missing=0 AND block_time_missing=0
        AND block_reset=0 AND block_ownership_gap=0 AND block_parser_gap=0
        AND block_required_invalid=0 AND block_model_unresolved=0 THEN 1 ELSE 0 END),
    CHECK (accounted_cached_tokens <= accounted_input_tokens),
    CHECK (accounted_cache_write_tokens IS NULL OR accounted_cached_tokens + accounted_cache_write_tokens <= accounted_input_tokens),
    CHECK (accounted_reasoning_tokens <= accounted_output_tokens),
    CHECK (accounted_total_tokens = accounted_input_tokens + accounted_output_tokens),
    CHECK ((start_total_input_tokens IS NULL AND start_total_cached_tokens IS NULL
        AND start_total_cache_write_tokens IS NULL AND start_total_output_tokens IS NULL
        AND start_total_reasoning_tokens IS NULL AND start_total_total_tokens IS NULL
        AND start_total_fingerprint IS NULL)
      OR (start_total_input_tokens IS NOT NULL AND start_total_cached_tokens IS NOT NULL
        AND start_total_output_tokens IS NOT NULL AND start_total_reasoning_tokens IS NOT NULL
        AND start_total_total_tokens IS NOT NULL AND start_total_fingerprint IS NOT NULL
        AND start_total_cached_tokens <= start_total_input_tokens
        AND start_total_reasoning_tokens <= start_total_output_tokens
        AND start_total_total_tokens = start_total_input_tokens + start_total_output_tokens
        AND (start_total_cache_write_tokens IS NULL OR start_total_cached_tokens + start_total_cache_write_tokens <= start_total_input_tokens))),
    CHECK ((last_total_input_tokens IS NULL AND last_total_cached_tokens IS NULL
        AND last_total_cache_write_tokens IS NULL AND last_total_output_tokens IS NULL
        AND last_total_reasoning_tokens IS NULL AND last_total_total_tokens IS NULL
        AND last_total_fingerprint IS NULL)
      OR (last_total_input_tokens IS NOT NULL AND last_total_cached_tokens IS NOT NULL
        AND last_total_output_tokens IS NOT NULL AND last_total_reasoning_tokens IS NOT NULL
        AND last_total_total_tokens IS NOT NULL AND last_total_fingerprint IS NOT NULL
        AND last_total_cached_tokens <= last_total_input_tokens
        AND last_total_reasoning_tokens <= last_total_output_tokens
        AND last_total_total_tokens = last_total_input_tokens + last_total_output_tokens
        AND (last_total_cache_write_tokens IS NULL OR last_total_cached_tokens + last_total_cache_write_tokens <= last_total_input_tokens)))
);

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

CREATE TABLE "codex_usage_event_occurrences" (
    source TEXT NOT NULL CHECK (source = 'codex'),
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    source_start_offset INTEGER NOT NULL CHECK (source_start_offset >= 0),
    source_end_offset INTEGER NOT NULL CHECK (source_end_offset > source_start_offset),
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (source, ledger_epoch, source_file_id, file_generation, source_start_offset),
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id),
    FOREIGN KEY (source, ledger_epoch, event_id)
        REFERENCES usage_events(source, source_epoch, event_id) DEFERRABLE INITIALLY DEFERRED
);

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

CREATE TABLE "codex_usage_session_quarantine" (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
    primary_error_code TEXT NOT NULL CHECK (length(primary_error_code) > 0),
    last_activity_at_ms INTEGER NOT NULL CHECK (last_activity_at_ms >= 0),
    first_seen_at_ms INTEGER NOT NULL CHECK (first_seen_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, root_session_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id)
);

CREATE TABLE "codex_usage_session_quarantine_sources" (
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
        REFERENCES "codex_usage_session_quarantine"(ledger_epoch, root_session_id) ON DELETE CASCADE,
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id)
);

CREATE TABLE "codex_usage_source_states" (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    source_file_id INTEGER NOT NULL,
    file_generation INTEGER NOT NULL CHECK (file_generation > 0),
    device_id INTEGER NOT NULL CHECK (device_id >= 0),
    inode INTEGER NOT NULL CHECK (inode >= 0),
    usage_parser_version INTEGER NOT NULL CHECK (usage_parser_version >= 0),
    canonical_algorithm_version INTEGER NOT NULL CHECK (canonical_algorithm_version >= 0),
    resolved_through_offset INTEGER NOT NULL CHECK (resolved_through_offset >= 0),
    observed_raw_size INTEGER NOT NULL CHECK (observed_raw_size >= 0),
    raw_tail_status TEXT NOT NULL CHECK (raw_tail_status IN ('unverified','none','half_line')),
    raw_tail_start_offset INTEGER CHECK (raw_tail_start_offset >= 0),
    owning_thread_id TEXT NOT NULL,
    root_session_id TEXT NOT NULL,
    continuation_state TEXT NOT NULL CHECK (continuation_state IN ('replayed_ancestor','owning_live')),
    previous_total_input_tokens INTEGER CHECK (previous_total_input_tokens >= 0),
    previous_total_cached_tokens INTEGER CHECK (previous_total_cached_tokens >= 0),
    previous_total_cache_write_tokens INTEGER CHECK (previous_total_cache_write_tokens >= 0),
    previous_total_output_tokens INTEGER CHECK (previous_total_output_tokens >= 0),
    previous_total_reasoning_tokens INTEGER CHECK (previous_total_reasoning_tokens >= 0),
    previous_total_total_tokens INTEGER CHECK (previous_total_total_tokens >= 0),
    previous_total_fingerprint BLOB,
    previous_total_offset INTEGER CHECK (previous_total_offset >= 0),
    chain_state TEXT NOT NULL CHECK (chain_state IN ('continuous','interrupted')),
    chain_block_reason TEXT CHECK (chain_block_reason IS NULL OR chain_block_reason IN ('malformed','oversized','total_invalid','ownership_gap','parser_gap')),
    active_turn_key TEXT,
    active_model TEXT,
    active_model_offset INTEGER CHECK (active_model_offset >= 0),
    active_reasoning_effort TEXT,
    active_reasoning_effort_offset INTEGER CHECK (active_reasoning_effort_offset >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0), reconciliation_state_json TEXT NOT NULL
    DEFAULT '{"version":1,"open_window_start_offset":null,"pending_response_ids":[],"modern_counter_domain":null,"modern_counter_total":null,"pending_evidence":[]}'
    CHECK (json_valid(reconciliation_state_json)),
    PRIMARY KEY (ledger_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES "codex_source_files"(source_file_id),
    FOREIGN KEY (owning_thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id),
    CHECK (resolved_through_offset <= observed_raw_size),
    CHECK ((active_model IS NULL) = (active_model_offset IS NULL)),
    CHECK ((active_reasoning_effort IS NULL) = (active_reasoning_effort_offset IS NULL)),
    CHECK ((previous_total_input_tokens IS NULL AND previous_total_cached_tokens IS NULL
        AND previous_total_cache_write_tokens IS NULL AND previous_total_output_tokens IS NULL
        AND previous_total_reasoning_tokens IS NULL AND previous_total_total_tokens IS NULL
        AND previous_total_fingerprint IS NULL AND previous_total_offset IS NULL)
      OR (previous_total_input_tokens IS NOT NULL AND previous_total_cached_tokens IS NOT NULL
        AND previous_total_output_tokens IS NOT NULL AND previous_total_reasoning_tokens IS NOT NULL
        AND previous_total_total_tokens IS NOT NULL AND previous_total_fingerprint IS NOT NULL
        AND previous_total_offset IS NOT NULL AND previous_total_offset <= resolved_through_offset
        AND previous_total_cached_tokens <= previous_total_input_tokens
        AND previous_total_reasoning_tokens <= previous_total_output_tokens
        AND previous_total_total_tokens = previous_total_input_tokens + previous_total_output_tokens
        AND (previous_total_cache_write_tokens IS NULL OR previous_total_cached_tokens + previous_total_cache_write_tokens <= previous_total_input_tokens)))
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

CREATE TABLE source_usage_epochs (
    source TEXT PRIMARY KEY CHECK (length(source) > 0),
    active_epoch INTEGER NOT NULL CHECK (active_epoch >= 0),
    build_epoch INTEGER CHECK (build_epoch >= 1),
    active_parser_version INTEGER NOT NULL CHECK (active_parser_version >= 0),
    build_parser_version INTEGER CHECK (build_parser_version >= 0),
    CHECK ((build_epoch IS NULL) = (build_parser_version IS NULL)),
    CHECK (build_epoch IS NULL OR build_epoch = active_epoch + 1)
);

CREATE TABLE "threads" (
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

CREATE TABLE "usage_events" (
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

CREATE INDEX antigravity_usage_quarantine_reason_seen_idx
    ON antigravity_usage_quarantine(reason_code, last_seen_at_ms);

CREATE INDEX codex_compaction_marker_response_idx
    ON codex_compaction_markers(ledger_epoch, owning_thread_id, response_id);

CREATE INDEX codex_compaction_marker_scope_idx
    ON codex_compaction_markers(ledger_epoch, root_session_id, owning_thread_id, occurred_at_ms);

CREATE INDEX codex_rollout_metadata_facts_thread_idx
    ON codex_rollout_metadata_facts(owning_thread_id);

CREATE INDEX codex_skill_usage_epoch_model_time_idx
    ON codex_skill_usage_events(ledger_epoch, model, occurred_at_ms);

CREATE INDEX codex_skill_usage_epoch_root_time_idx
    ON codex_skill_usage_events(ledger_epoch, root_session_id, occurred_at_ms);

CREATE INDEX codex_skill_usage_epoch_source_start_idx
    ON codex_skill_usage_events(ledger_epoch, source_file_id, source_start_offset);

CREATE INDEX codex_skill_usage_epoch_time_idx
    ON codex_skill_usage_events(ledger_epoch, occurred_at_ms);

CREATE INDEX codex_source_checkpoints_status_idx
    ON codex_source_checkpoints(consumer_kind, processing_status);

CREATE INDEX codex_source_files_status_idx
    ON codex_source_files(file_status);

CREATE INDEX codex_source_files_thread_idx
    ON codex_source_files(thread_id);

CREATE INDEX codex_usage_build_sources_status_idx
    ON codex_usage_build_sources(build_epoch, completion_status);

CREATE INDEX codex_usage_event_hold_event_idx
    ON codex_usage_event_holds(source, ledger_epoch, event_id);

CREATE INDEX codex_usage_event_occurrences_event_idx
    ON codex_usage_event_occurrences(source, ledger_epoch, event_id);

CREATE INDEX codex_usage_event_occurrences_source_idx
    ON codex_usage_event_occurrences(
        source, ledger_epoch, source_file_id, file_generation, source_start_offset
    );

CREATE UNIQUE INDEX codex_usage_response_identity_idx
    ON codex_usage_event_facts(source, ledger_epoch, owning_thread_id, response_id)
    WHERE response_id IS NOT NULL;

CREATE INDEX codex_usage_session_quarantine_epoch_idx
    ON codex_usage_session_quarantine(ledger_epoch);

CREATE INDEX codex_usage_session_quarantine_sources_source_idx
    ON codex_usage_session_quarantine_sources(ledger_epoch, source_file_id);

CREATE INDEX codex_usage_window_thread_idx
    ON codex_usage_reconciliation_windows(ledger_epoch, owning_thread_id, turn_key);

CREATE INDEX idx_scan_runs_state ON scan_runs(state);

CREATE INDEX source_scan_runs_scan_idx
    ON source_scan_runs(scan_id, source);

CREATE INDEX threads_parent_idx ON threads(parent_thread_id);

CREATE INDEX threads_root_idx ON threads(root_session_id);

CREATE INDEX threads_source_updated_idx ON threads(source, updated_at_ms);

CREATE INDEX threads_updated_idx ON threads(updated_at_ms);

CREATE INDEX usage_events_model_time_idx
    ON usage_events(source, source_epoch, model, occurred_at_ms);

CREATE INDEX usage_events_occurred_time_idx
    ON usage_events(occurred_at_ms);

CREATE INDEX usage_events_root_time_idx
    ON usage_events(source, source_epoch, root_session_id, occurred_at_ms);

CREATE INDEX usage_events_thread_time_idx
    ON usage_events(source, source_epoch, thread_id, occurred_at_ms);

CREATE INDEX usage_events_time_idx
    ON usage_events(source, source_epoch, occurred_at_ms);

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

CREATE TRIGGER scan_runs_terminal_state_guard
BEFORE UPDATE OF state ON scan_runs
WHEN OLD.state IN ('completed', 'failed', 'start_failed')
 AND NEW.state IN ('queued', 'running')
BEGIN
    SELECT RAISE(ABORT, 'terminal scan cannot be restarted');
END;

CREATE TABLE schema_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    version INTEGER NOT NULL CHECK (version >= 1)
);

INSERT INTO schema_meta(id, generation, version) VALUES (1, 1, 1);
INSERT INTO app_meta(id, data_revision, status_revision, scan_state,
                     cost_algorithm_version, pricing_catalog_version)
VALUES (1, 0, 0, 'idle', 0, 0);
INSERT INTO codex_adapter_state(id, home_fingerprint, binding_status)
VALUES (1, NULL, 'unbound');
INSERT INTO source_usage_epochs(source, active_epoch, build_epoch,
                                active_parser_version, build_parser_version)
VALUES ('codex', 0, NULL, 0, NULL);

PRAGMA user_version = 1000;
