# Operations Runbook (Current Bootstrap API)

Administrative examples use the development bootstrap token from the process environment; never put it in a URL or commit it. Production administrators use `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD` by default; Passkeys are optional credentials bound after login. The operator declares and maintains the upstream keys and is responsible for their intended use; the gateway does not verify upstream contractual permission. Registration is closed by default. This runbook covers operator-maintained upstream pools/keys, users and Gateway Keys. See [Local quota, usage, and billing](quota-and-billing.md) for quota assignment, account-isolated request history, cleanup, local ledger, redemptions, and disabled-by-default payment sandbox operations.

```sh
export GATEWAY_ADMIN_TOKEN='...'
export BASE_URL='http://127.0.0.1:8787'

# Create an operator-declared usage boundary for a Context7 account or Teamspace.
curl -sS -X POST "$BASE_URL/api/admin/pools" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"teamspace-main","authorization_scope":"operator-declared-teamspace"}'

# Add an upstream key (the raw value is encrypted at rest and not returned).
curl -sS -X POST "$BASE_URL/api/admin/pools/$POOL_ID/keys" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"api_key":"<context7-api-key>"}'

# Validate with a fast Context7 read request. Only a successful validation marks the key active.
curl -sS -X POST "$BASE_URL/api/admin/pools/$POOL_ID/keys/$KEY_ID/validate" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN"

# Create a user bound to that pool, then issue a one-time Gateway Key.
curl -sS -X POST "$BASE_URL/api/admin/users" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"display_name":"example-user","upstream_pool_id":"'"$POOL_ID"'"}'
curl -sS -X POST "$BASE_URL/api/admin/users/$USER_ID/tokens" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"laptop"}'
```

The final create response contains the Gateway Key one time. Store it safely for the user; subsequent list calls return only its public prefix/status. Revoke the token immediately if it is disclosed. The pool's `authorization_scope` is an operator-maintained label describing the account/Teamspace boundary; the service records it but does not verify upstream contractual permission. A Context7 `401` marks the key invalid, `403` disables its pool pending operator review, and `429` cools down the whole pool for the upstream `Retry-After` duration. Re-enable a pool disabled by `403` only after understanding the permission/plan response; never rotate or add keys to bypass a cooldown.

`GET /api/admin/pools/$POOL_ID/keys/$KEY_ID/usage?days=1` is a separate, admin-only upstream snapshot probe. It stores the raw upstream usage object with its fetch time and never exposes it to Gateway users. A 403/404 is reported as `unsupported` rather than invented; local Gateway credits remain a separate ledger.

## Bulk key import

`POST /api/admin/pools/$POOL_ID/keys/bulk` imports up to 100 upstream Context7 keys in one request. The route has a 64 KiB body limit, and malformed JSON is rejected as `invalid_request` without echoing the submitted body. The response reports `created`, `duplicates` and `invalid` totals, plus a `results` array with each entry's index and status (`created`, `duplicate` or `invalid`); it never returns a raw key value.

```sh
curl -sS -X POST "$BASE_URL/api/admin/pools/$POOL_ID/keys/bulk" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"api_keys":["ctx7sk-...","ctx7sk-..."]}'
```

Imported keys start in `pending` status and do not carry traffic. Verify each imported key with `POST /api/admin/pools/$POOL_ID/keys/$KEY_ID/validate`; only a successful validation changes it to `active`. Duplicate or invalid entries are reported per item, and the per-item report never echoes key material. The admin console also shows the status for each input line and keeps the original textarea content whenever any line is not created, so failed entries are not lost. Empty requests and requests with more than 100 entries are rejected; split larger imports into batches.

## Registration control

Registration is closed by default. Opening it is an administrator action; the service rejects `registration_open=true` unless `DEFAULT_UPSTREAM_POOL_ID` names an enabled pool with at least one schedulable key (status `active`, or `rate_limited` with an expired cooldown) and the pool is not cooling down or circuit-broken, and unless Linux.do OAuth plus the encryption key and token pepper are configured. This is a runtime configuration check, not an upstream permission review. Closing registration stops new sign-ups; it does not revoke existing sessions.

## Health and diagnostics

- `/health/live`: process liveness.
- `/health/ready`: database connectivity and migration readiness.
- `x-gateway-request-id`: safe correlation identifier returned on each request.
- Usage rows are internal metadata only; no query/body/response data is retained.

## Alerting baseline

There is no bundled metrics server; expose these as log/metric queries in the deployment's monitoring stack. Suggested initial thresholds (tune against traffic):

| Signal | Suggested page threshold |
| --- | --- |
| Ready check failing or DB writes failing | 2 consecutive failures |
| Gateway HTTP 5xx ratio | >2% over 5 minutes, excluding client 4xx |
| Upstream 401/403 | any occurrence: inspect key/pool permission and scope immediately |
| Upstream 429 | sustained >10 minutes or repeated after cooldown |
| Available upstream keys in a pool | 0, or below one for >5 minutes |
| Local `quota_exceeded` ratio | >20% for a user over 15 minutes (possible abuse or wrong plan) |
| p95 proxy latency | >30s for 10 minutes (excluding known long client streams) |
| Administrator login/registration failures | 5 in 10 minutes from one source |
| Sandbox payment signature failures | any occurrence (provider should be disabled in production) |
| Disk free / SQLite errors | <15% free or any `SQLITE_FULL`/I/O error |

Session/cookie brute force is bounded by short-lived server-side ceremonies and per-purpose caps; put a TLS reverse proxy in front of the service and add source-IP request limits there for `/auth/linuxdo`, `/api/admin/auth/*` and `/mcp`. Never trust a client-supplied forwarded-for header without a proxy allowlist.

## Remaining production drills

Before relying on a live deployment, and preferably before enabling open registration: run one controlled 401/403/429/5xx sequence against the real deployment plan, confirm pool cooldown/breaker recovery, exercise a legacy MCP session close, restore a production-like backup, and repeat the load test used to size the single-node SQLite deployment. These drills are not replaced by the repository's unit/mock tests.
