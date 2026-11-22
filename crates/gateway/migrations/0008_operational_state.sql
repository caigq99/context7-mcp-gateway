ALTER TABLE upstream_pools ADD COLUMN breaker_until TEXT;
ALTER TABLE upstream_pools ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE upstream_keys ADD COLUMN encryption_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE upstream_keys ADD COLUMN credential_fingerprint BLOB;
ALTER TABLE upstream_keys ADD COLUMN in_flight INTEGER NOT NULL DEFAULT 0 CHECK (in_flight >= 0);
CREATE UNIQUE INDEX upstream_keys_active_fingerprint
    ON upstream_keys(credential_fingerprint) WHERE deleted_at IS NULL;

CREATE TABLE gateway_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    registration_open INTEGER NOT NULL DEFAULT 0 CHECK (registration_open IN (0, 1))
);
INSERT INTO gateway_settings(id) VALUES (1);

CREATE TABLE announcements (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    ends_at INTEGER
);

CREATE TABLE upstream_usage_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    key_id TEXT NOT NULL REFERENCES upstream_keys(id),
    days INTEGER NOT NULL,
    usage_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX usage_snapshots_key_time ON upstream_usage_snapshots(key_id, fetched_at DESC);

CREATE TABLE mcp_resume_bookmarks (
    session_id TEXT NOT NULL REFERENCES mcp_sessions(id) ON DELETE CASCADE,
    event_digest BLOB NOT NULL CHECK (length(event_digest) = 32),
    reservation_id TEXT NOT NULL REFERENCES quota_reservations(id) ON DELETE CASCADE,
    rpc_id_json TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, event_digest)
);
CREATE INDEX mcp_resume_bookmarks_expiry ON mcp_resume_bookmarks(expires_at);

UPDATE app_metadata SET value = '8', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
