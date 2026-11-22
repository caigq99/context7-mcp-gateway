CREATE TABLE user_quota_assignments (
    user_id TEXT NOT NULL REFERENCES users(id),
    version INTEGER NOT NULL CHECK (version > 0),
    requests_per_five_minutes INTEGER NOT NULL CHECK (requests_per_five_minutes >= 0),
    business_calls_per_hour INTEGER NOT NULL CHECK (business_calls_per_hour >= 0),
    daily_units INTEGER NOT NULL CHECK (daily_units >= 0),
    monthly_units INTEGER NOT NULL CHECK (monthly_units >= 0),
    weights TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, version)
) WITHOUT ROWID;

ALTER TABLE usage_events ADD COLUMN started_unix INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN admitted INTEGER NOT NULL DEFAULT 1 CHECK (admitted IN (0, 1));

-- Legacy operation labels could contain arbitrary client input. Normalize these before exposing history.
UPDATE usage_events SET started_unix = COALESCE(unixepoch(started_at), 0),
    operation = CASE operation
        WHEN 'tools/call' THEN 'mcp_tools_call'
        WHEN 'initialize' THEN 'mcp_initialize'
        WHEN 'tools/list' THEN 'mcp_tools_list'
        WHEN 'ping' THEN 'mcp_ping'
        WHEN 'context7.rest.read' THEN 'rest_other'
        ELSE 'mcp_other'
    END,
    failure_code = CASE WHEN gateway_status BETWEEN 200 AND 299 THEN NULL ELSE 'upstream_error' END;

CREATE INDEX usage_events_time_id ON usage_events(started_unix DESC, id DESC);
CREATE INDEX usage_events_user_time_id ON usage_events(user_id, started_unix DESC, id DESC);

CREATE TABLE quota_counters (
    user_id TEXT NOT NULL REFERENCES users(id),
    period TEXT NOT NULL CHECK (period IN ('hour', 'day', 'month')),
    window_start INTEGER NOT NULL,
    operation TEXT NOT NULL,
    requests INTEGER NOT NULL DEFAULT 0 CHECK (requests >= 0),
    business_calls INTEGER NOT NULL DEFAULT 0 CHECK (business_calls >= 0),
    successful INTEGER NOT NULL DEFAULT 0 CHECK (successful >= 0),
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    units INTEGER NOT NULL DEFAULT 0 CHECK (units >= 0),
    quota_rejections INTEGER NOT NULL DEFAULT 0 CHECK (quota_rejections >= 0),
    PRIMARY KEY (user_id, period, window_start, operation)
) WITHOUT ROWID;

CREATE INDEX quota_counters_period_time ON quota_counters(period, window_start);

WITH periods(name) AS (VALUES ('hour'), ('day'), ('month')),
normalized AS (
    SELECT user_id, operation, gateway_status, success_units, failure_code, name,
        CASE name
            WHEN 'hour' THEN started_unix - started_unix % 3600
            WHEN 'day' THEN started_unix - started_unix % 86400
            WHEN 'month' THEN unixepoch(started_unix, 'unixepoch', 'start of month')
        END AS window_start
    FROM usage_events CROSS JOIN periods
)
INSERT INTO quota_counters (user_id, period, window_start, operation, requests, business_calls, successful, failed, units)
SELECT user_id, name, window_start, operation, COUNT(*), SUM(operation IN (
    'mcp_tools_call', 'rest_other', 'search_v3', 'fetch_v3', 'library_search_v2', 'context_v2'
)),
    SUM(failure_code IS NULL), SUM(failure_code IS NOT NULL), SUM(success_units)
FROM normalized GROUP BY user_id, name, window_start, operation;

CREATE TABLE quota_reservations (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    gateway_token_id TEXT NOT NULL REFERENCES gateway_tokens(id),
    pool_id TEXT NOT NULL REFERENCES upstream_pools(id),
    upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id),
    operation TEXT NOT NULL,
    request_id TEXT NOT NULL,
    started_unix INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > started_unix),
    hour_start INTEGER NOT NULL,
    day_start INTEGER NOT NULL,
    month_start INTEGER NOT NULL,
    units INTEGER NOT NULL CHECK (units >= 0)
);

CREATE INDEX quota_reservations_user_time ON quota_reservations(user_id, started_unix);
CREATE INDEX quota_reservations_expiry ON quota_reservations(expires_at);

UPDATE app_metadata SET value = '6', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
