# Architecture

## Components

```text
MCP / Context7 read-only REST client
  -> Axum request ID + path-only structured tracing
  -> Gateway Key parse/digest/scope/expiry/user-state validation
  -> fixed route/query allowlist + MCP protocol gate
  -> user-bound operator-declared pool selection (in-flight-aware LRU)
  -> atomic local rate/quota reservation
  -> short-lived upstream-key lease + decrypt with ChaCha20-Poly1305
  -> fixed Context7 MCP / REST origin (redirects disabled)
  -> streamed JSON/SSE response and selective response headers
  -> exactly-once usage/credit settlement, with legacy event resume mapping

React/Vite static client console       React/Vite administrator console
Linux.do Connect OAuth + user cookie  Password login + optional WebAuthn + admin-only cookie
SQLite single-node migrations: accounts, keys, sessions, quota, orders, audit
```

## Identity and credential boundaries

- Every user is assigned to a single upstream pool. A downstream Gateway Key cannot choose its account, pool, Context7 key or upstream host.
- Operator Context7 keys are AEAD-encrypted at rest and bound to their pool/key identifiers. The test key itself and all key material are absent from the repository, UI response bodies, logs and usage records.
- Gateway keys use high-entropy secrets whose HMAC-SHA-256 digests are stored with an independent server pepper. An additional token-ID-bound ChaCha20-Poly1305 ciphertext lets the signed-in owner copy the key on demand without rendering the full value. Lists show only prefixes and scopes. Users cannot create, rotate or revoke keys from their console. Scope, expiry, token revocation and account disable are enforced before proxying.
- User login uses OAuth state, an HttpOnly SameSite cookie and PKCE; self-registration is closed by default. Administrator login defaults to the environment-configured username/password pair, with an Argon2id in-memory hash and version-bound password sessions. Passkeys are optional WebAuthn credentials bound after login; passkey-only deployments retain the host CLI enrollment/recovery flow. Production rejects the development-only admin bearer.
- Browser mutations require same-origin checks; public client proxy calls use an Authorization bearer instead of browser cookies. API responses are no-store. Authorization, Cookie, query text, MCP body and response content are not added to HTTP logs.

## Proxy contract

- REST facade routes map one-to-one to fixed official Context7 origins and strict per-route parameter allowlists. The client cannot provide a host, path, proxy or authorization header.
- MCP JSON-RPC bodies pass through. New `2026-07-28` header/body metadata is checked; legacy `2025-11-25` initialize, notifications, session IDs, SSE and Last-Event-ID are supported. Only legacy upstream sessions are remapped into opaque Gateway IDs bound to the creating Gateway token and original key.
- Each accepted request reserves local quota atomically before sending upstream and obtains a key lease. Context7 `401` invalidates a key, `403` disables the affected pool pending review, `429` cools that pool using Retry-After, and repeated 5xx responses trip a temporary circuit breaker. A request sent to Context7 is never retried onto another key.
- JSON and SSE response paths settle business units once, distinguish JSON-RPC errors from HTTP success, and allow legacy resumed streams to share one reservation without duplicate business charges. Cancellation, transport failure and lease expiry settle as failures.

## Storage, quotas and local credit

SQLite via SQLx migrations is the single-node authoritative store. Atomic `BEGIN IMMEDIATE` quota reservations protect the five-minute request window, hourly business-call cap, daily/monthly local unit budgets and optional local credit lots. Successful calls consume units; failed calls release them. Monthly plan updates are versioned; usage events retain only identifiers, operation, timestamp, status, duration and request correlation. Event retention is configurable from 1–365 days; aggregate counters and audit history are not removed by that setting.

Local redemptions, orders and credit ledger represent only Gateway units. The optional signed payment callback fixture is non-production only and disabled unless explicitly configured. It is not an upstream Context7 billing integration.

## Deployment boundary

The supported target is one Rust/Axum process backed by one persistent SQLite volume, behind a TLS-terminating reverse proxy. Do not run multiple replicas against the same SQLite file or publish the container's plain HTTP port to an untrusted network. Back up the database with the documented SQLite online backup scripts and separately preserve the exact encryption key and token pepper.

For development and tests, `CONTEXT7_MCP_ORIGIN` / `CONTEXT7_REST_ORIGIN` may point the fixed upstream client at a loopback Context7 stand-in. Production rejects non-empty overrides and always contacts the documented origins. This switch is configuration-only; callers still cannot supply a host, path or proxy.
