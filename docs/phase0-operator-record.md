# Phase 0 Operator Declaration Record

> Status: **DECLARED** — 2026-11-22. The operator declares that they maintain the upstream Context7 API keys and are responsible for their intended use. The repository does not verify upstream contractual permissions. Registration remains closed by default and may be opened only after the runtime prerequisites in §4 are met. No raw key material is recorded in this document.

## 1. Operator declaration

| Field | Value |
| --- | --- |
| Operator | Repository operator (self-declared) |
| Public origin | Deployment-specific HTTPS origin configured through `PUBLIC_ORIGIN`; not asserted by this repository |
| Intended user population | Users admitted by the operator through the configured login adapter; registration is closed by default |
| Target pools | The pool referenced by `DEFAULT_UPSTREAM_POOL_ID` and any additional pools created by the operator |
| Registration decision | Closed by default. An administrator may open registration only when the configured default pool has an eligible active key and the OAuth and cryptographic prerequisites are configured. |
| Date (UTC) | 2026-11-22 |

## 2. Key source and pool declaration

| Field | Value |
| --- | --- |
| Key source | Context7 API keys purchased or otherwise maintained by the operator and entered directly into the upstream pool. The repository does not verify how they were obtained or the contract under which they are used. |
| Count by pool | Maintained by the operator. Current counts and statuses are visible in the administrator pool listing; this record intentionally contains no key values, hashes, or per-key inventory. |

| Pool | Intended use | Key source | Declared count |
| --- | --- | --- | --- |
| `DEFAULT_UPSTREAM_POOL_ID` (configured default) | Default pool for registration and general gateway traffic | Operator-maintained Context7 keys | Operator-maintained; see the administrator pool listing |
| Other operator-created pools | Uses declared by the operator for each pool | Operator-maintained Context7 keys | Operator-maintained; see the administrator pool listing |

## 3. Responsible-use declaration

The operator declares that:

- each upstream key is used only within the account/Teamspace and user scope the operator intends;
- upstream `401`, `403` and `429` responses, including `Retry-After` and rate-limit headers, are honored;
- keys are not rotated, replaced, or selected to evade upstream rate limits, plan quotas, bans, or access controls;
- pools are not used to mix unrelated accounts or Teamspaces into an undeclared shared boundary;
- registration and pool access may be limited or closed at any time if the operator's intended use changes.

## 4. Runtime registration prerequisites

Registration is closed by default. An administrator can open it only when all of the following are true:

- `DEFAULT_UPSTREAM_POOL_ID` names an enabled pool;
- that pool contains at least one schedulable key (status `active`, or `rate_limited` with an expired cooldown), and the pool itself is not cooling down or circuit-broken;
- Linux.do OAuth, `GATEWAY_ENCRYPTION_KEY`, and `GATEWAY_TOKEN_PEPPER` are configured.

These are runtime configuration checks. They do not verify upstream contractual permission and do not replace the operator's declaration.

## 5. Contract verification (non-blocking)

The upstream contract checks recorded in [`context7-upstream-contract.md`](context7-upstream-contract.md) are deployment-readiness evidence for API/MCP behavior, error handling, and rate-limit handling. Open items there are non-blocking for the operator's declaration; production-specific negative drills and deployment observations still need to be run on the target host.

## 6. Record maintenance

Update this record when the operator, public origin, user population, target pools, key source, or registration decision changes. Keep key material out of this record and out of any copied evidence.
