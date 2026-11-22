# Operations Runbook (Current Bootstrap API)

Administrative examples use the development bootstrap token from the process environment; never put it in a URL or commit it. Production administrators authenticate with a registered Passkey. This runbook covers approved upstream pools/keys, users and Gateway Keys. See [Local quota, usage, and billing](quota-and-billing.md) for quota assignment, account-isolated request history, cleanup, local ledger, redemptions, and disabled-by-default payment sandbox operations.

```sh
export GATEWAY_ADMIN_TOKEN='...'
export BASE_URL='http://127.0.0.1:8787'

# Create an authorization/usage boundary for a Context7 account or Teamspace.
curl -sS -X POST "$BASE_URL/api/admin/pools" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"teamspace-main","authorization_scope":"approved-teamspace-identifier"}'

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
  -d '{"display_name":"approved-user","upstream_pool_id":"'"$POOL_ID"'"}'
curl -sS -X POST "$BASE_URL/api/admin/users/$USER_ID/tokens" \
  -H "Authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"laptop"}'
```

The final create response contains the Gateway Key one time. Store it safely for the user; subsequent list calls return only its public prefix/status. Revoke the token immediately if it is disclosed. A Context7 403 disables its pool for operator review; re-enable the pool only after understanding the authorization/plan response. A 429 cools down the whole pool for the upstream `Retry-After` duration.

`GET /api/admin/pools/$POOL_ID/keys/$KEY_ID/usage?days=1` is a separate, admin-only upstream snapshot probe. It stores the raw upstream usage object with its fetch time and never exposes it to Gateway users. A 403/404 is reported as `unsupported` rather than invented; local Gateway credits remain a separate ledger.

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
| Upstream 401/403 | any occurrence: inspect key/pool authorization immediately |
| Upstream 429 | sustained >10 minutes or repeated after cooldown |
| Available upstream keys in an authorized pool | 0, or below one for >5 minutes |
| Local `quota_exceeded` ratio | >20% for a user over 15 minutes (possible abuse or wrong plan) |
| p95 proxy latency | >30s for 10 minutes (excluding known long client streams) |
| Administrator login/registration failures | 5 in 10 minutes from one source |
| Sandbox payment signature failures | any occurrence (provider should be disabled in production) |
| Disk free / SQLite errors | <15% free or any `SQLITE_FULL`/I/O error |

Session/cookie brute force is bounded by short-lived server-side ceremonies and per-purpose caps; put a TLS reverse proxy in front of the service and add source-IP request limits there for `/auth/linuxdo`, `/api/admin/auth/*` and `/mcp`. Never trust a client-supplied forwarded-for header without a proxy allowlist.

## Remaining production drills

Before enabling open registration: run one controlled 401/403/429/5xx sequence against the real deployment plan, confirm pool cooldown/breaker recovery, exercise a legacy MCP session close, restore a production-like backup, and repeat the load test used to size the single-node SQLite deployment. These drills are not replaced by the repository's unit/mock tests.
