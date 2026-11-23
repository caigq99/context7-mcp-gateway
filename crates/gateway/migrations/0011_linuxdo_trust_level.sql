ALTER TABLE users ADD COLUMN trust_level INTEGER
    CHECK (trust_level IS NULL OR (trust_level BETWEEN 0 AND 4));

UPDATE app_metadata SET value = '11', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
