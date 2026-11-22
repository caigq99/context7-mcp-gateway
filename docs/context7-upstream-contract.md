# Context7 Upstream Contract Notes

> Status: successful live operator-key tests and local Gateway-through-upstream smoke are recorded. This is not an official guarantee of future behavior, plan allowance or permission to share keys. Controlled negative-status, session-resume and production deployment tests remain open.

## Observed successful contract

The smoke test on **November 22, 2026 (Asia/Shanghai)** used the operator-provided test credential interactively (the credential was not logged or placed in the repository). Direct upstream calls returned `200` for `GET /api/v2/libs/search`, `GET /api/v2/context`, `GET /api/v3/search`, `GET /api/v3/fetch` and `GET /api/v2/usage?days=1`. The observed response fields and accepted parameters are in `scripts/smoke_context7.py`.

The same calls were then repeated through the local Gateway using a short-lived Gateway Key: all four REST paths returned `200`; MCP `2026-07-28` `tools/list` returned JSON `200`; legacy `2025-11-25` `initialize` and `notifications/initialized` completed; legacy `tools/list`, `resolve-library-id` and `query-docs` returned SSE `200` and valid matching JSON-RPC result IDs. The local smoke checked return status and response shape but deliberately did not print documentation content.

## Routes and parameters

- Remote MCP: `https://mcp.context7.com/mcp`; standard tools are discovered dynamically through `tools/list`. The Gateway does not insert tools into upstream responses.
- REST origins are fixed to `https://context7.com`; client keys, arbitrary paths and hosts are not forwarded.
- `GET /api/v2/libs/search`: `libraryName`, `query`, optional version/language/fast.
- `GET /api/v2/context`: `libraryId`, `query`, optional language/type.
- `GET /api/v3/search`: required `query`, up to four repeated `library` hints, optional version/language/type. `version` requires a library hint.
- `GET /api/v3/fetch`: required `url`, optional question `query`, `tokens` (100–10000), `type`, or `offset` when `query` is absent.
- `GET /api/v2/usage`: admin-only snapshot probe; upstream data is stored separately from local Gateway credits and limits.

Upstream auth is `Authorization: Bearer <Context7 API key>`. The Gateway independently authenticates downstream `c7g_...` Gateway Keys, and only the selected encrypted upstream key is put in the upstream Authorization header.

## Protocol behavior and restrictions

MCP new transport validates the `2026-07-28` protocol metadata, `Mcp-Method` and operation name tuple. It does not generate sessions for that transport. Legacy `2025-11-25` transport remaps opaque session identifiers and binds them to the creating Gateway token and upstream key. The live smoke observed SSE but no upstream-issued session; the binding, cross-token rejection, SSE resume and exactly-once business settlement of that path are now covered end to end against a loopback Context7 stand-in (`proxy::mock_tests`).

The code treats upstream `401` as an invalid key, `403` as a permission error that pauses the affected pool for operator review, `429` as a pool cooldown honoring `Retry-After` (later callers get a local `429`), and three consecutive `5xx` as a `503` circuit breaker for the pool. All of these transitions, including "no upstream request while cooled/opened", are exercised by the loopback mock tests. Production still needs one operator-controlled negative test per deployed plan.

## Remaining Phase 0 evidence

- Collect operator documentation that the upstream account/Teamspace permits the intended user population and proxy sharing.
- Session deletion and abrupt live disconnect behavior should still be observed once against the real service; the mock covers resume, cross-token isolation and no duplicate charge.
- Validate any deployment-specific plan limits, billing/teamspace observations and client compatibility before opening registration.
- The Docker image build is not yet verified because the image registry download stalled in this environment.

## Public source references

- [Context7 API Guide](https://context7.com/docs/api-guide)
- [Context7 MCP README](https://github.com/upstash/context7/blob/master/packages/mcp/README.md)
- [Context7 API Key guide](https://context7.com/docs/howto/api-keys)
- [Context7 API usage reference](https://context7.com/docs/api-reference/usage/get-api-usage)
- [MCP 2026-07-28 Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [MCP 2025-11-25 Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
