# Context7 MCP Gateway

Context7 MCP Gateway is a self-hostable, Rust-based gateway for Context7. The operator maintains the upstream Context7 API-key pool; users access Context7 through Gateway Keys issued by this service. The goal is to implement the product capabilities described in [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md), not to reproduce another product's UI.

## Implementation status

The first end-to-end implementation is in place: Context7 MCP and four read-only REST proxies; encrypted operator-maintained upstream keys; Gateway Key scopes, expiry, rotation and revocation; Linux.do OAuth/PKCE user sessions; Passkey-only production administration; atomic local quota accounting and request history; user/admin consoles; client setup guides; announcements; and local quota-credit, redemption and sandbox order flows. See the phase status and remaining verification gates in [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md).

**This is not an authorization approval or an automatic production go-live.** Before sharing an upstream pool with other users, confirm in writing that the maintained Context7 account/Teamspace and the intended audience are permitted. The repository intentionally disables production payment providers and account registration by default; production operators must configure a valid default pool and explicitly open registration after review. A Docker image build and an independent security review remain release gates.

The Phase 0 authorization gate in the plan must be satisfied before operating a shared upstream pool for external users. Keys must be used within the permitted account/Teamspace and user scope; key rotation must not be used to evade upstream limits.

## Development prerequisites

- Rust 1.89+ (current source uses edition 2024).
- Node.js 24+ and npm.
- SQLite is bundled through SQLx; a system SQLite installation is not required.

## Run locally

```sh
# Terminal 1: configure separate development secrets and start the API
export GATEWAY_ADMIN_TOKEN="replace-with-a-random-secret-of-at-least-32-characters"
export GATEWAY_ENCRYPTION_KEY="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
export GATEWAY_TOKEN_PEPPER="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
cargo run --locked -p context7-gateway

# Terminal 2: frontend development server
cd web
npm ci
npm run dev
```

The API listens on `127.0.0.1:8787`; Vite listens on `127.0.0.1:5173` and proxies API/health/MCP paths to the backend. Database and static paths can be changed with environment variables. Use `.env.example` as a reference; do not store real secrets in the repository. For the administrator browser console, use the one-time enrollment command in [`docs/admin-auth.md`](docs/admin-auth.md), then sign in with a Passkey at `/admin/login`. `GATEWAY_ADMIN_TOKEN` is accepted only for non-production API/bootstrap development; production startup rejects it. Configure Linux.do OAuth, the upstream pool, production secrets, and TLS separately as described in [`docs/deployment.md`](docs/deployment.md).

## Checks

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cd web && npm ci && npm run build
```

## Docs

- [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md): phase-by-phase scope, architecture decisions, interfaces, and acceptance criteria.
- [`docs/architecture.md`](docs/architecture.md): component boundaries and request flow.
- [`docs/security.md`](docs/security.md): secrets, auth boundaries, threat model, and logging defaults.
- [`docs/deployment.md`](docs/deployment.md): local/container deployment and secret/backup handling.
- [`docs/operations.md`](docs/operations.md): current bootstrap workflows and planned operator procedures.
- [`docs/client-setup.md`](docs/client-setup.md): Gateway Key, MCP and REST usage.
- [`docs/context7-upstream-contract.md`](docs/context7-upstream-contract.md): observed API/MCP contracts and remaining untested error cases.
- [`docs/admin-auth.md`](docs/admin-auth.md): Passkey onboarding/recovery.
- [`docs/quota-and-billing.md`](docs/quota-and-billing.md): quota, usage, credits, redemptions and disabled payment sandbox.
