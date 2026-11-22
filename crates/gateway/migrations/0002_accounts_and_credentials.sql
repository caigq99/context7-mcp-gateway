CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL,
    oauth_issuer TEXT,
    oauth_subject TEXT,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_login_at TEXT,
    UNIQUE (oauth_issuer, oauth_subject)
);

CREATE TABLE auth_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    session_digest BLOB NOT NULL UNIQUE CHECK (length(session_digest) = 32),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE INDEX auth_sessions_user_id ON auth_sessions(user_id, expires_at);

CREATE TABLE upstream_pools (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    authorization_scope TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE upstream_keys (
    id TEXT PRIMARY KEY NOT NULL,
    pool_id TEXT NOT NULL REFERENCES upstream_pools(id),
    ciphertext BLOB NOT NULL,
    nonce BLOB NOT NULL CHECK (length(nonce) = 12),
    display_prefix TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'disabled', 'invalid', 'permission_error', 'rate_limited')),
    last_checked_at TEXT,
    last_success_at TEXT,
    last_failure_at TEXT,
    rate_limited_until TEXT,
    last_used_at TEXT,
    deleted_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX upstream_keys_pool_status_lru
    ON upstream_keys(pool_id, status, last_used_at, created_at)
    WHERE deleted_at IS NULL;

CREATE TABLE gateway_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    token_digest BLOB NOT NULL UNIQUE CHECK (length(token_digest) = 32),
    prefix TEXT NOT NULL,
    name TEXT NOT NULL,
    scopes_json TEXT NOT NULL DEFAULT '["mcp","context7:read"]',
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    expires_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE INDEX gateway_tokens_user_status ON gateway_tokens(user_id, status, created_at);

CREATE TABLE admin_audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    object_type TEXT NOT NULL,
    object_id TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    request_id TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX admin_audit_events_created_at ON admin_audit_events(created_at DESC);

UPDATE app_metadata SET value = '2', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
