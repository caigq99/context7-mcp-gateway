CREATE TABLE admin_account (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    user_handle BLOB NOT NULL CHECK (length(user_handle) = 16)
);

INSERT INTO admin_account(singleton, user_handle) VALUES (1, randomblob(16));

CREATE TABLE admin_passkeys (
    credential_id BLOB PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    credential_json TEXT NOT NULL,
    signature_counter INTEGER NOT NULL DEFAULT 0 CHECK (signature_counter >= 0),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_used_at INTEGER,
    revoked_at INTEGER
);

CREATE TABLE admin_sessions (
    digest BLOB PRIMARY KEY NOT NULL CHECK (length(digest) = 32),
    credential_id BLOB NOT NULL REFERENCES admin_passkeys(credential_id),
    origin TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE INDEX admin_sessions_expiry ON admin_sessions(expires_at);

CREATE TABLE admin_enrollment_tickets (
    digest BLOB PRIMARY KEY NOT NULL CHECK (length(digest) = 32),
    mode TEXT NOT NULL CHECK (mode IN ('initial', 'recovery')),
    expires_at INTEGER NOT NULL
);

CREATE TABLE admin_ceremonies (
    digest BLOB PRIMARY KEY NOT NULL CHECK (length(digest) = 32),
    purpose TEXT NOT NULL CHECK (purpose IN ('login', 'registration')),
    state_json TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX admin_ceremonies_expiry ON admin_ceremonies(expires_at);

UPDATE app_metadata SET value = '5', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
