ALTER TABLE users ADD COLUMN upstream_pool_id TEXT REFERENCES upstream_pools(id);
ALTER TABLE upstream_pools ADD COLUMN rate_limited_until TEXT;

CREATE TABLE mcp_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    external_session_digest BLOB NOT NULL UNIQUE CHECK (length(external_session_digest) = 32),
    gateway_token_id TEXT NOT NULL REFERENCES gateway_tokens(id),
    pool_id TEXT NOT NULL REFERENCES upstream_pools(id),
    upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id),
    upstream_session_ciphertext BLOB NOT NULL,
    upstream_session_nonce BLOB NOT NULL CHECK (length(upstream_session_nonce) = 12),
    expires_at TEXT NOT NULL,
    closed_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX mcp_sessions_token_expiry ON mcp_sessions(gateway_token_id, expires_at);

CREATE TABLE usage_events (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    gateway_token_id TEXT NOT NULL REFERENCES gateway_tokens(id),
    pool_id TEXT REFERENCES upstream_pools(id),
    upstream_key_id TEXT REFERENCES upstream_keys(id),
    operation TEXT NOT NULL,
    request_id TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    duration_ms INTEGER NOT NULL DEFAULT 0,
    gateway_status INTEGER NOT NULL,
    upstream_status INTEGER,
    success_units INTEGER NOT NULL DEFAULT 0,
    failure_code TEXT
);

CREATE INDEX usage_events_user_time ON usage_events(user_id, started_at DESC);
CREATE INDEX usage_events_token_time ON usage_events(gateway_token_id, started_at DESC);

UPDATE app_metadata SET value = '3', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
