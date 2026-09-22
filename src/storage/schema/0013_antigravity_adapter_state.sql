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

CREATE INDEX antigravity_usage_quarantine_reason_seen_idx
    ON antigravity_usage_quarantine(reason_code, last_seen_at_ms);
