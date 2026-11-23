ALTER TABLE gateway_tokens ADD COLUMN token_ciphertext BLOB;
ALTER TABLE gateway_tokens ADD COLUMN token_nonce BLOB;
ALTER TABLE gateway_tokens ADD COLUMN token_encryption_version INTEGER;

UPDATE app_metadata SET value = '10', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
