-- Usagi schema version 10: durable metadata ordering and relationship quality.
-- The migration runner executes this script inside one BEGIN IMMEDIATE transaction.

ALTER TABLE rollout_metadata_facts
    ADD COLUMN latest_context_turn_id TEXT;

ALTER TABLE rollout_metadata_facts
    ADD COLUMN relationship_conflict INTEGER NOT NULL DEFAULT 0
        CHECK (relationship_conflict IN (0, 1));
