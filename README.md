# Context7 MCP Gateway

Context7 MCP Gateway is a self-hostable, Rust-based gateway for Context7. The operator maintains the upstream Context7 API-key pool; users access Context7 through Gateway Keys issued by this service. The goal is to implement the product capabilities described in [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md), not to reproduce another product's UI.

## Implementation status

The first end-to-end implementation is in place: Context7 MCP and four read-only REST proxies; encrypted operator-maintained upstream keys with single and bulk import; Gateway Key scopes, expiry, rotation and revocation; Linux.do OAuth/PKCE user sessions; Passkey-only production administration; atomic local quota accounting and request history; user/admin consoles; client setup guides; announcements; and local quota-credit, redemption and sandbox order flows. See the phase status and remaining verification work in [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md).

**The operator declares and maintains the upstream keys; this repository does not verify upstream contractual permissions.** The operator is responsible for using each key within its intended account/Teamspace and user scope. Registration remains closed by default and is opened only by an administrator after the configured default pool has an eligible active key and the OAuth and cryptographic configuration prerequisites are met. The repository intentionally disables production payment providers; the image build and a hardened Compose smoke are verified in this workspace, while an independent security review and deployment-specific drills remain release gates.

Phase 0 is an operator declaration plus upstream contract verification, recorded in [`docs/phase0-operator-record.md`](docs/phase0-operator-record.md); it is not a blocking external review. Regardless of registration state, upstream `401`/`403`/`429` responses are honored, and keys must not be rotated to evade upstream rate limits or access controls.

## Development prerequisites

- Rust 1.89+ (current source uses edition 2024).
- Node.js 24+ and npm.
- SQLite is bundled through SQLx; a system SQLite installation is not required.

## Run locally

For a single-process preview that also supports administrator Passkeys, build the frontend once and serve it from the Rust process:

```sh
cd web
npm ci
npm run build
cd ..

export APP_ENV=development
export PUBLIC_ORIGIN="http://localhost:8787"
export GATEWAY_ADMIN_TOKEN="$(openssl rand -hex 32)"
export GATEWAY_ENCRYPTION_KEY="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
export GATEWAY_TOKEN_PEPPER="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
cargo run --locked -p context7-gateway
```

Open `http://localhost:8787/console` and `http://localhost:8787/admin/login`. Use the literal hostname `localhost`, not `127.0.0.1`, when testing Passkeys. Issue the one-time administrator enrollment code in another terminal with the same `APP_ENV`, `PUBLIC_ORIGIN`, and `DATABASE_PATH`:

```sh
cargo run --locked -p context7-gateway -- admin issue-enrollment
```

Keep `GATEWAY_ENCRYPTION_KEY` and `GATEWAY_TOKEN_PEPPER` stable across restarts; changing either value makes previously stored upstream Keys or Gateway Keys unusable.

For frontend hot reload, run the API with the Vite origin and then start Vite:

```sh
# Terminal 1
export APP_ENV=development
export PUBLIC_ORIGIN="http://localhost:5173"
export GATEWAY_ADMIN_TOKEN="$(openssl rand -hex 32)"
export GATEWAY_ENCRYPTION_KEY="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
export GATEWAY_TOKEN_PEPPER="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
cargo run --locked -p context7-gateway

# Terminal 2: frontend development server
cd web
npm ci
npm run dev
```

The API listens on `127.0.0.1:8787`; Vite listens on `127.0.0.1:5173` and proxies API/health/MCP paths to the backend. Open `http://localhost:5173`. Database and static paths can be changed with environment variables. Use `.env.example` as a reference; do not store real secrets in the repository. For the administrator browser console, use the one-time enrollment command in [`docs/admin-auth.md`](docs/admin-auth.md), then sign in with a Passkey at `/admin/login`. `GATEWAY_ADMIN_TOKEN` is accepted only for non-production API/bootstrap development; production startup rejects it. Configure Linux.do OAuth, the upstream pool, production secrets, and TLS separately as described in [`docs/deployment.md`](docs/deployment.md).

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
- [`docs/linuxdo-oauth.md`](docs/linuxdo-oauth.md): Linux.do Connect application, callback, environment, registration gate and troubleshooting.
- [`docs/operations.md`](docs/operations.md): current bootstrap workflows and planned operator procedures.
- [`docs/client-setup.md`](docs/client-setup.md): Gateway Key, MCP and REST usage.
- [`docs/context7-upstream-contract.md`](docs/context7-upstream-contract.md): observed API/MCP contracts and remaining untested error cases.
- [`docs/phase0-operator-record.md`](docs/phase0-operator-record.md): operator declaration of upstream key responsibility and the runtime registration prerequisites.
- [`docs/admin-auth.md`](docs/admin-auth.md): Passkey onboarding/recovery.
- [`docs/quota-and-billing.md`](docs/quota-and-billing.md): quota, usage, credits, redemptions and disabled payment sandbox.
