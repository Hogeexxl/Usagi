-- Usagi schema version 6: durable Subagent agent_path metadata.
--
-- The migration runner executes this file inside one BEGIN IMMEDIATE
-- transaction.  Rebuild the metadata fact table so every existing field,
-- constraint, foreign key, and index remains explicit.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE rollout_metadata_facts_v6 (
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
    agent_path TEXT,
    agent_path_provenance TEXT CHECK (
        agent_path_provenance IS NULL
        OR agent_path_provenance IN ('session_meta', 'thread_spawn')
    ),
    agent_path_record_offset INTEGER CHECK (
        agent_path_record_offset IS NULL OR agent_path_record_offset >= 0
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
        (agent_path IS NULL
            AND agent_path_provenance IS NULL
            AND agent_path_record_offset IS NULL)
        OR (agent_path IS NOT NULL
            AND agent_path_provenance IS NOT NULL
            AND agent_path_record_offset IS NOT NULL)
    ),
    CHECK (continuation_state <> 'owning_live' OR ownership_confidence = 'confirmed'),
    CHECK (created_at_ms IS NULL OR created_at_ms >= 0),
    CHECK (latest_context_at_ms IS NULL OR latest_context_at_ms >= 0),
    CHECK (updated_at_ms >= 0),
    FOREIGN KEY (source_file_id) REFERENCES source_files(source_file_id) ON DELETE CASCADE
);

INSERT INTO rollout_metadata_facts_v6 (
    source_file_id, file_generation, metadata_parser_version,
    resolved_through_offset, owning_thread_id, continuation_state,
    cwd, cwd_provenance, cwd_record_offset, created_at_ms,
    latest_context_model, latest_context_at_ms,
    parent_thread_id_hint, parent_hint_provenance, parent_hint_record_offset,
    agent_role_hint, agent_role_provenance, agent_role_record_offset,
    agent_path, agent_path_provenance, agent_path_record_offset,
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
    NULL, NULL, NULL,
    replay_start_offset, owning_records_start_offset,
    ownership_confidence, fact_quality_status, updated_at_ms
FROM rollout_metadata_facts;

DROP TABLE rollout_metadata_facts;
ALTER TABLE rollout_metadata_facts_v6 RENAME TO rollout_metadata_facts;

CREATE INDEX rollout_metadata_facts_thread_idx
    ON rollout_metadata_facts(owning_thread_id);
