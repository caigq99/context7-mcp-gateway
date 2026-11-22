CREATE TABLE oauth_states (
    state_digest BLOB PRIMARY KEY NOT NULL CHECK (length(state_digest) = 32),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at TEXT
);

CREATE INDEX oauth_states_expiry ON oauth_states(expires_at);

UPDATE app_metadata SET value = '4', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
