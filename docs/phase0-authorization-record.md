# Phase 0 Authorization and Go/No-Go Record

> Status: **NOT APPROVED — template only.** Do not open registration or share an upstream pool until the operator completes and signs this record. The presence of a Context7 API key does not by itself grant permission to proxy it to others.

## 1. Scope

| Field | Value |
| --- | --- |
| Service owner / operator | |
| Public gateway origin | |
| Intended user population | e.g. named organization members / Linux.do trust level ≥ N / closed beta list |
| Expected maximum users | |
| Geographic/legal scope | |
| Data retention decision (`GATEWAY_EVENT_RETENTION_DAYS`) | |
| Payment enabled? | No (recommended) / provider + approval reference |

## 2. Upstream authorization

| Field | Value |
| --- | --- |
| Context7 account / organization | |
| Teamspace / plan | |
| Upstream key owner and custodian | |
| Number of keys in each authorized pool | |
| How keys were obtained | |
| Written permission / terms reference for sharing with the user population | |
| Restrictions on model, data, source policy or concurrent usage | |
| Upstream contact / support case (if any) | |

Attach the relevant agreement, email, ticket or internal approval with secrets redacted. Do not paste raw Context7 keys into this record.

## 3. Contract verification evidence

| Check | Evidence / date | Result |
| --- | --- | --- |
| Auth header and four read REST routes | `scripts/smoke_context7.py` (direct) | |
| MCP 2026-07-28 `tools/list` + standard tools | smoke output | |
| MCP 2025-11-25 initialize / notification / tools | smoke output | |
| Gateway-issued key through local `/mcp` and `/api/context7/*` | `scripts/smoke_context7.py --gateway-origin ...` | |
| Upstream 401/403/429/5xx and breaker behavior | loopback mock tests + one deployment-plan negative drill | |
| Legacy session creation / isolation / close | mock tests + one deployed client session | |
| Upstream usage endpoint shape and plan visibility | admin usage screenshot / JSON with secrets redacted | |
| TLS, reverse proxy timeouts and SSE behavior | deployment test | |

## 4. Go/No-Go decision

| Decision | Selection |
| --- | --- |
| Phase 0 authorization gate | ☐ GO  ☐ NO-GO |
| Approved user population | |
| Approved pools (IDs / labels) | |
| Conditions or expiry of approval | |
| Operator name | |
| Date (UTC) | |
| Review evidence location | |

A **NO-GO** means: keep registration closed, use only a local mock or the operator's own single-account testing, and do not expose upstream keys to other users. Re-run this record when the scope or upstream plan changes.
