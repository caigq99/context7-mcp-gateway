CREATE TABLE admin_password_sessions (
    digest BLOB PRIMARY KEY NOT NULL CHECK (length(digest) = 32),
    account_version BLOB NOT NULL CHECK (length(account_version) = 32),
    origin TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE INDEX admin_password_sessions_expiry
    ON admin_password_sessions(expires_at);

UPDATE app_metadata SET value = '9', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
