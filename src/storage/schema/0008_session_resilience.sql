-- Usagi schema version 8: resilient metadata continuation and Session quarantine.
-- The migration runner executes this script inside one BEGIN IMMEDIATE transaction.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE rollout_metadata_facts_v8 (
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
    updated_at_ms INTEGER NOT NULL,
    CHECK ((cwd IS NULL AND cwd_provenance IS NULL AND cwd_record_offset IS NULL) OR (cwd IS NOT NULL AND cwd_provenance IS NOT NULL AND cwd_record_offset IS NOT NULL)),
    CHECK ((parent_thread_id_hint IS NULL AND parent_hint_provenance IS NULL AND parent_hint_record_offset IS NULL) OR (parent_thread_id_hint IS NOT NULL AND parent_hint_provenance IS NOT NULL AND parent_hint_record_offset IS NOT NULL)),
    CHECK ((agent_role_hint IS NULL AND agent_role_provenance IS NULL AND agent_role_record_offset IS NULL) OR (agent_role_hint IS NOT NULL AND agent_role_provenance IS NOT NULL AND agent_role_record_offset IS NOT NULL)),
    CHECK ((agent_path IS NULL AND agent_path_provenance IS NULL AND agent_path_record_offset IS NULL) OR (agent_path IS NOT NULL AND agent_path_provenance IS NOT NULL AND agent_path_record_offset IS NOT NULL)),
    CHECK (continuation_state NOT IN ('replayed_ancestor','owning_live') OR ownership_confidence = 'confirmed'),
    CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    CHECK (latest_context_at_ms IS NULL OR latest_context_at_ms >= 0),
    CHECK (updated_at_ms >= 0),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id) ON DELETE CASCADE
);
INSERT INTO rollout_metadata_facts_v8 (
    source_file_id,file_generation,metadata_parser_version,resolved_through_offset,owning_thread_id,continuation_state,
    cwd,cwd_provenance,cwd_record_offset,created_at_ms,latest_context_model,latest_context_at_ms,
    parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,agent_role_hint,agent_role_provenance,
    agent_role_record_offset,agent_path,agent_path_provenance,agent_path_record_offset,replay_start_offset,
    owning_records_start_offset,ownership_confidence,fact_quality_status,updated_at_ms
)
SELECT source_file_id,file_generation,metadata_parser_version,resolved_through_offset,owning_thread_id,continuation_state,
    cwd,cwd_provenance,cwd_record_offset,created_at_ms,latest_context_model,latest_context_at_ms,
    parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,agent_role_hint,agent_role_provenance,
    agent_role_record_offset,agent_path,agent_path_provenance,agent_path_record_offset,replay_start_offset,
    owning_records_start_offset,ownership_confidence,fact_quality_status,updated_at_ms
FROM rollout_metadata_facts;
DROP TABLE rollout_metadata_facts;
ALTER TABLE rollout_metadata_facts_v8 RENAME TO rollout_metadata_facts;
CREATE INDEX rollout_metadata_facts_thread_idx ON rollout_metadata_facts(owning_thread_id);

CREATE TABLE usage_build_sources_v8 (
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
    carry_phase TEXT NOT NULL CHECK (carry_phase IN ('none','occurrences','turns','anomalies','finalize')),
    carry_after_start_offset INTEGER CHECK (carry_after_start_offset >= 0),
    carry_after_turn_key TEXT,
    carry_after_anomaly_id TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (build_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
    CHECK (required_generation = expected_file_generation),
    CHECK (required_through_offset <= observed_raw_size),
    CHECK ((raw_tail_status='unverified' AND raw_tail_start_offset IS NULL) OR (raw_tail_status='none' AND raw_tail_start_offset IS NULL AND required_through_offset=observed_raw_size) OR (raw_tail_status='half_line' AND raw_tail_start_offset=required_through_offset AND required_through_offset<observed_raw_size)),
    CHECK ((completion_status IN ('pending','blocked','quarantined') AND completed_generation IS NULL AND completed_through_offset IS NULL) OR (completion_status IN ('rebuilt','carried') AND completed_generation=required_generation AND completed_through_offset IS NOT NULL AND completed_through_offset>=required_through_offset)),
    CHECK ((completion_status IN ('blocked','quarantined')) = (completion_error_code IS NOT NULL)),
    CHECK (completion_status <> 'quarantined' OR expected_root_session_id IS NOT NULL),
    CHECK ((carry_phase='none' AND carry_from_epoch IS NULL AND carry_after_start_offset IS NULL AND carry_after_turn_key IS NULL AND carry_after_anomaly_id IS NULL) OR (carry_phase<>'none' AND carry_from_epoch IS NOT NULL))
);
INSERT INTO usage_build_sources_v8 SELECT * FROM usage_build_sources;
DROP TABLE usage_build_sources;
ALTER TABLE usage_build_sources_v8 RENAME TO usage_build_sources;
CREATE INDEX usage_build_sources_status_idx ON usage_build_sources(build_epoch, completion_status);

CREATE TABLE usage_session_quarantine (
    ledger_epoch INTEGER NOT NULL CHECK (ledger_epoch > 0),
    root_session_id TEXT NOT NULL CHECK (length(root_session_id) > 0),
    primary_error_code TEXT NOT NULL CHECK (length(primary_error_code) > 0),
    last_activity_at_ms INTEGER NOT NULL CHECK (last_activity_at_ms >= 0),
    first_seen_at_ms INTEGER NOT NULL CHECK (first_seen_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, root_session_id),
    FOREIGN KEY (root_session_id) REFERENCES threads(thread_id)
);
CREATE INDEX usage_session_quarantine_epoch_idx ON usage_session_quarantine(ledger_epoch);

CREATE TABLE usage_session_quarantine_sources (
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
        REFERENCES usage_session_quarantine(ledger_epoch, root_session_id) ON DELETE CASCADE,
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id)
);
CREATE INDEX usage_session_quarantine_sources_source_idx
    ON usage_session_quarantine_sources(ledger_epoch, source_file_id);


-- Usage source checkpoints can also end safely while a confirmed fork is still
-- replaying its ancestor. Rebuild the table because SQLite cannot widen CHECK.
ALTER TABLE usage_source_states RENAME TO usage_source_states_v7;
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
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (ledger_epoch, source_file_id),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
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
INSERT INTO usage_source_states SELECT * FROM usage_source_states_v7;
DROP TABLE usage_source_states_v7;
