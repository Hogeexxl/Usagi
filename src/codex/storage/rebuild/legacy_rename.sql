ALTER TABLE source_files RENAME TO codex_source_files;
ALTER TABLE source_checkpoints RENAME TO codex_source_checkpoints;
ALTER TABLE usage_event_occurrences RENAME TO codex_usage_event_occurrences;
ALTER TABLE usage_source_states RENAME TO codex_usage_source_states;
ALTER TABLE usage_build_sources RENAME TO codex_usage_build_sources;
ALTER TABLE skill_usage_events RENAME TO codex_skill_usage_events;
ALTER TABLE turns RENAME TO codex_turns;
ALTER TABLE ingest_anomalies RENAME TO codex_ingest_anomalies;
