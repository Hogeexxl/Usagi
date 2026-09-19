-- Usagi schema version 5: stable normalized Thread project assignment.
--
-- The migration runner temporarily disables foreign-key enforcement while
-- this table is rebuilt, then validates every relationship before commit.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE threads_v5 (
    thread_id TEXT PRIMARY KEY CHECK (length(thread_id) > 0),
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

INSERT INTO threads_v5 (
    thread_id, parent_thread_id, root_session_id, agent_role,
    title, project_name, project_path, project_kind, metadata_model,
    created_at_ms, updated_at_ms, archived, current_rollout_path,
    metadata_quality_status, metadata_resolved_at_ms
)
SELECT
    thread_id, parent_thread_id, root_session_id, agent_role,
    title, project_name, project_path,
    CASE
        WHEN project_path IS NOT NULL AND length(project_path) > 0 THEN 'project'
        ELSE 'unknown'
    END,
    metadata_model,
    created_at_ms, updated_at_ms, archived, current_rollout_path,
    metadata_quality_status, metadata_resolved_at_ms
FROM threads;

DROP TABLE threads;
ALTER TABLE threads_v5 RENAME TO threads;

CREATE INDEX threads_parent_idx ON threads(parent_thread_id);
CREATE INDEX threads_root_idx ON threads(root_session_id);
CREATE INDEX threads_updated_idx ON threads(updated_at_ms);
