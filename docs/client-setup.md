# Client Setup (Current Gateway Contract)

The gateway exposes a remote MCP endpoint at `https://<gateway-origin>/mcp` and a small Context7 HTTP read-only façade under `/api/context7`. Use the Gateway Key issued by the operator, not a raw Context7 key:

```text
Authorization: Bearer <gateway-key>
```

Keep the value in the MCP client's secret/environment-variable facility. Do not place a token in the URL, shell history, screenshots, or shared configuration files.

## MCP clients

Configure the client to use an HTTP MCP server URL ending in `/mcp` and attach the Gateway Bearer header above. The proxy validates the `2026-07-28` method/name/protocol headers and passes through legacy `2025-11-25` initialize, notification, SSE and session behavior. Use the generated, copy-ready examples on the signed-in console's Setup page for Codex, Claude, VS Code, Cursor, Windsurf and generic MCP clients. Session resume is implemented but still needs a controlled upstream session test before relying on it in production.

## HTTP REST clients

The read-only routes are:

- `GET /api/context7/v3/search`
- `GET /api/context7/v3/fetch`
- `GET /api/context7/v2/libs/search`
- `GET /api/context7/v2/context`

The gateway maps these to the corresponding official Context7 paths, validates query parameter allowlists, attaches the selected upstream credential, and returns upstream status/content. `api_key`, arbitrary URL hosts, and unrecognized query parameters are rejected.

The proxy smoke script `scripts/smoke_context7.py --gateway-origin http://127.0.0.1:8787` requests the key interactively and tests all four read routes plus MCP tool discovery/calls without printing the supplied secret. Use local HTTP only for loopback development; production requires TLS. Context7 query/plan limits belong to the upstream account; Gateway's local usage/credits and optional sandbox payment path are separate and do not represent an official Context7 plan.
