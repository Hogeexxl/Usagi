-- Usagi schema version 7: durable reasoning context and derived cost.
-- The migration runner executes this script in its migration transaction.

PRAGMA defer_foreign_keys = ON;

ALTER TABLE usage_events ADD COLUMN reasoning_effort TEXT;
ALTER TABLE usage_events ADD COLUMN estimated_cost_nanos_usd INTEGER
    CHECK (estimated_cost_nanos_usd IS NULL OR estimated_cost_nanos_usd >= 0);

ALTER TABLE app_meta ADD COLUMN cost_algorithm_version INTEGER NOT NULL DEFAULT 0
    CHECK (cost_algorithm_version >= 0);
ALTER TABLE app_meta ADD COLUMN pricing_catalog_version INTEGER NOT NULL DEFAULT 0
    CHECK (pricing_catalog_version >= 0);

ALTER TABLE usage_source_states RENAME TO usage_source_states_v6;
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
    previous_total_cached_tokens INTEGER CHECK (previous_total_cached_tokens >= 0),
    previous_total_cache_write_tokens INTEGER CHECK (previous_total_cache_write_tokens >= 0),
    previous_total_output_tokens INTEGER CHECK (previous_total_output_tokens >= 0),
    previous_total_reasoning_tokens INTEGER CHECK (previous_total_reasoning_tokens >= 0),
    previous_total_total_tokens INTEGER CHECK (previous_total_total_tokens >= 0),
    previous_total_fingerprint BLOB,
    previous_total_offset INTEGER CHECK (previous_total_offset >= 0),
    chain_state TEXT NOT NULL CHECK (chain_state IN ('continuous', 'interrupted')),
    chain_block_reason TEXT CHECK (chain_block_reason IS NULL OR chain_block_reason IN
        ('malformed', 'oversized', 'total_invalid', 'ownership_gap', 'parser_gap')),
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
INSERT INTO usage_source_states (
    ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,
    canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
    raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
    previous_total_input_tokens,previous_total_cached_tokens,previous_total_cache_write_tokens,
    previous_total_output_tokens,previous_total_reasoning_tokens,previous_total_total_tokens,
    previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
    active_turn_key,active_model,active_model_offset,active_reasoning_effort,
    active_reasoning_effort_offset,updated_at_ms
)
SELECT ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,
    canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
    raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
    previous_total_input_tokens,previous_total_cached_tokens,previous_total_cache_write_tokens,
    previous_total_output_tokens,previous_total_reasoning_tokens,previous_total_total_tokens,
    previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
    active_turn_key,active_model,active_model_offset,NULL,NULL,updated_at_ms
FROM usage_source_states_v6;
DROP TABLE usage_source_states_v6;

ALTER TABLE turns RENAME TO turns_v6;
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
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id),
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
INSERT INTO turns (
    ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,started_at_ms,ended_at_ms,
    start_offset,end_offset,status,start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
    start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
    last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,last_total_output_tokens,
    last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,accounted_input_tokens,
    accounted_cached_tokens,accounted_cache_write_tokens,accounted_output_tokens,accounted_reasoning_tokens,
    accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,single_model,
    unresolved_model_seen,reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
    compensation_allowed,block_start_missing,block_time_missing,block_reset,block_ownership_gap,block_parser_gap,
    block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms
)
SELECT ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,started_at_ms,ended_at_ms,
    start_offset,end_offset,status,start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
    start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
    last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,last_total_output_tokens,
    last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,accounted_input_tokens,
    accounted_cached_tokens,accounted_cache_write_tokens,accounted_output_tokens,accounted_reasoning_tokens,
    accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,single_model,
    unresolved_model_seen,'none',NULL,0,compensation_allowed,block_start_missing,block_time_missing,block_reset,
    block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,quality_status,
    state_through_offset,updated_at_ms
FROM turns_v6;
DROP TABLE turns_v6;

