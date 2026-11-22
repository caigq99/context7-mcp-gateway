# Context7 MCP Gateway — 分阶段实现方案

> 文档状态：实施基线（v1）  
> 编写日期：2026-11-21  
> 项目状态：核心代码与控制台已落地；Phase 0 授权记录、受控错误/会话测试、Docker 镜像验证和独立安全评审仍是生产发布 Gate。  
> 产品方向：运营者自行维护获准使用的 Context7 上游 API Key 池；用户只使用本服务签发的 Gateway API Key。

## 1. 目标与边界

本项目建设一个具备 Tavily Hikari 同类产品能力的 Context7 网关：统一接入 Context7 MCP/文档查询 API，管理上游密钥，向用户签发独立 Gateway Key，并提供账户、配额、用量、请求记录、接入指南和管理员控制台。参考的是产品功能与运营流程，不要求照搬 Hikari 的视觉设计或内部实现。

### 已确定的产品决策

1. **上游凭证由运营者维护**：管理员维护多把 Context7 API Key；普通用户不需要也不应看到 Context7 Key。
2. **用户凭证由网关签发**：用户拿到本服务的 Gateway Key，以 `Authorization: Bearer <gateway-key>` 访问 MCP/HTTP 接口。
3. **默认接入面**：Context7 远程 MCP (`/mcp`) 与官方文档化的只读 REST 查询接口；不重建 Context7 的索引、爬取或解析系统。
4. **上下游账务分离**：网关本地用量/配额和 Context7 上游用量/账单分别显示、分别存档，不将本地额度描述成 Context7 官方额度。
5. **数据最小化**：默认不持久化用户原始查询文本、完整 MCP 报文或文档响应正文。
6. **首期不实现 Context7 管理功能**：不代理 Admin MCP、不管理用户的 Context7 Teamspace、来源、策略或文档库写操作。
7. **可选支付**：充值仅增加本服务本地额度；支付适配器默认关闭，不能代表或代替 Context7 官方计费。

### 上线前置条件（发布阻断项）

Context7 官方资料明确区分 API Key、账户/Teamspace 使用量和访问权限，并限制未经授权的 Key 分享、转售或绕过上游限制。上线前必须由运营者确认：所维护的 Key 及其订阅/Teamspace **允许用于目标用户群和拟提供的网关服务**。本项目不以轮换 Key 的方式躲避 `429`、套餐配额、封禁或访问控制；如果无法确认获准范围，仍可做本地开发和单用户验证，但不得开放多租户生产服务。该条件作为 Phase 0 的正式 Go/No-Go gate，而不是上线后的免责声明。

## 2. 与参考产品的功能映射

| 参考产品能力 | Context7 Gateway 的对应实现 | 首期状态 |
|---|---|---|
| 上游 API Key 池 | 管理员增删/启停/验证 Context7 Key；按授权的池/账户范围调度；前端只显示脱敏值 | 必须 |
| 下游访问 Token | 用户创建、一次性复制、撤销/重建 Gateway Key；服务端只存不可逆摘要 | 必须 |
| MCP 网关 | 代理官方 Context7 MCP；转发 Streamable HTTP/JSON-RPC 及必要的 SSE/session 语义 | 必须 |
| HTTP API 网关 | 代理官方文档化的 Search、Fetch、Library Search、Context、Usage 等只读接口 | 必须 |
| 使用量/配额 | 用户本地配额、使用统计；管理员配置计划与额度；上游用量另行展示 | 必须 |
| 请求日志 | 用户查看自身调用记录；管理员全局过滤/排障；脱敏、限期保留 | 必须 |
| 用户登录与账户 | Linux.do Connect OAuth 登录作为初始默认适配器；登录能力与 Gateway Key 生命周期分离 | 必须 |
| 管理员控制台 | 单独的管理员认证与 RBAC；管理上游 Key、用户、配额、公告、设置 | 必须 |
| 客户端安装指南 | Codex、Claude Code/Desktop、VS Code/Copilot、Cursor、Windsurf、通用 MCP 和 REST 示例 | 必须 |
| 充值与订单 | 本地 Gateway 额度报价、订单、回调验签、额度记账；Linux.do Credit 可选适配 | 后续阶段 |
| 高可用/多节点 | 单节点可靠部署先行；有明确需求与负载证据后再做共享数据库/分布式限流 | 暂不包含 |

## 3. 上游接口与行为基线

实现前以 Context7 官方文档与 MCP README 为准，不以 Hikari 的 Tavily API 面貌假设 Context7 能力：

- 远程 MCP 地址为 `https://mcp.context7.com/mcp`。当前 MCP Streamable HTTP 规范版本为 `2026-07-28`，仍需兼容上游/客户端使用的 `2025-11-25` 旧式 initialize/session 流程；实现必须以双方实际支持的版本为准，不得假定所有客户端或 Context7 上游已升级。官方常规工具包括 `resolve-library-id` 与 `query-docs`。其他代码搜索工具属于可选/实验能力，必须以上游当前 `tools/list` 响应为准，不在网关中伪造或固定添加。
- MCP 网关必须按传输版本透明代理，不重写 JSON-RPC body，不强制注入新版协议元数据。2026-07-28 新式交互使用 POST 与 `Mcp-Method` / `Mcp-Name` / `Mcp-Param-*` 元数据，并可能采用可选 session；2025-11-25 旧式交互使用 `initialize`、`MCP-Session-Id`，并可能使用 GET/SSE/DELETE。转发 `Mcp-*` 未知扩展头、`MCP-Protocol-Version`、`Last-Event-ID` 和服务器协商内容；仅旧式上游 session 需要不透明 ID 重映射和上游 Key 绑定。
- REST 文档列有 `GET /api/v3/search`、`GET /api/v3/fetch`、`GET /api/v2/libs/search`、`GET /api/v2/context`、`GET /api/v2/usage` 等接口。实现前由契约测试核验请求参数、响应类型和鉴权方式，接口版本应集中配置，避免散落在代码中。
- Context7 API 使用其 Bearer API Key 鉴权。网关收到用户 Gateway Key 后先完成本地鉴权/配额判断，再选择获准的上游 Key 并构造上游请求；不得把上游 Key 返回给客户端或写入日志。
- 正确保留上游 HTTP 状态码、MCP JSON-RPC 错误、限流信息及 `Retry-After` / `RateLimit-*` 等有意义响应头；网关可以补充自己的错误 ID，但不得把上游限额错误伪装成成功。
- **Key 调度不等于限额绕过**：调度只发生在管理员声明的同一获准池内。若一把 Key 返回 429，先按上游 `Retry-After` 将其限流；不得仅为越过该限额而立刻换另一把 Key。只有运营授权策略明确允许、且池中的其他 Key 属于可独立使用的获准容量时才可继续选取；不满足时向客户端返回明确 429。
- HTTP `fetch` 虽接受目标 URL，网关仍只调用固定的 Context7 官方 API 主机，不允许用户指定上游主机、代理地址、任意路径或任意 Header，防止网关变成开放代理。

## 4. 技术与运行架构

### 默认技术栈

- **后端/网关**：Rust stable、Axum、Tokio、SQLx；提供静态文件和 `/api/*`、`/mcp` 路由。
- **数据存储**：SQLite + SQLx migration，单节点持久化部署；状态文件通过 Docker volume 持久保存。首期不宣称多副本/HA。
- **前端**：React + TypeScript + Vite；用户控制台与管理员控制台可共用组件和设计系统，但路由及数据权限隔离。
- **交付**：Docker 镜像、Docker Compose 示例、健康检查、版本信息、数据库备份/恢复说明。

### 请求流

```text
MCP/HTTP Client
  -> Gateway Key 鉴权
  -> 用户/Token 状态与本地配额校验
  -> 固定 Context7 路由与操作白名单
  -> 获准 Key 池调度 + 上游熔断/限流状态
  -> Context7 MCP 或 REST 上游
  -> 透传协议结果/状态/限流信息
  -> 异步写入最小化 usage event 与审计元数据
```

### 安全和秘密管理约定

- Context7 上游 Key 使用应用层认证加密后存储；生产密钥由部署平台 Secret/KMS 注入，数据库仅持密文、Key Hint 和密钥版本。密钥缺失、解密失败或版本不匹配时必须 fail closed。
- Gateway Key 使用密码学安全随机数生成；数据库保存带服务端 pepper 的 HMAC-SHA-256 摘要、不可逆前缀、用户关联、状态、权限范围和创建/使用时间。完整值仅创建成功时返回一次；重置即生成新值并撤销旧值。
- Key 管理、用户管理、额度变更、订单记账等管理操作均写追加式管理员审计事件；审计事件不包含秘密值。
- 管理员认证与用户 OAuth 会话分开；生产管理员路径默认启用 Passkey/WebAuthn，另提供受控的恢复路径；不提供无认证生产模式。
- 不允许通过 URL query string 传递 Gateway Key；客户端示例使用环境变量占位符，防止 URL、浏览器历史或代理访问日志意外留存凭证。

## 5. 数据模型基线

所有表使用迁移版本管理，时间统一存 UTC；保留 `created_at`、`updated_at` 和软删除/状态字段以便审计。

| 实体 | 核心字段/约束 |
|---|---|
| `users` | `id`、OAuth issuer/subject 唯一键、昵称、状态、创建时间、最后登录时间 |
| `auth_sessions` / `oauth_states` / `admin_passkeys` | 用户与管理员会话分开存储；OAuth state 一次性、短时有效；管理员 Passkey credential ID、公钥材料、计数器和注册时间 |
| `upstream_pools` | `id`、名称、授权/账务范围标签、调度策略、启用状态；池不可跨越未获准的账户/Teamspace 边界 |
| `upstream_keys` | `id`、`pool_id`、密文、nonce/key version、展示用前缀、状态(`active/disabled/invalid/permission_error/rate_limited`)、最近验证/成功/失败时间、限流截止时间、上次使用时间 |
| `gateway_tokens` | `id`、`user_id`、摘要、唯一前缀、名称/备注、权限范围、状态、可选过期时间、创建/最后使用/撤销时间 |
| `mcp_sessions` | 仅旧式上游 session 使用：Gateway session ID 摘要、所属 Token、绑定的上游 Key、加密保存的上游 session ID、过期时间、关闭时间；session 只能被创建它的 Gateway Token 使用。新式 MCP 无上游 session 时不创建记录 |
| `quota_plans` / `user_quota_assignments` | 管理员定义的本地请求速率、小时/日/月业务单位额度、有效期和计划版本；配额默认拒绝超额请求 |
| `usage_events` | `id`、用户/Token/池/上游 Key ID、操作名、开始时间、耗时、网关状态、上游状态、计费单位、请求关联 ID；不包含原始 body/响应 |
| `quota_counters` | 用户/窗口/操作的聚合计数及窗口起止；使用事务或原子 upsert，避免并发超额 |
| `admin_audit_events` | 操作者、动作、对象 ID、时间、结果、脱敏前后值摘要、关联请求 ID |
| `announcements` | 管理员发布的公告、受众范围、发布时间/结束时间、启用状态 |
| `recharge_orders`（Phase 6） | 用户、报价版本、金额/支付币种、状态、外部订单号唯一键、回调验证结果、额度账本引用；支付回调具幂等约束 |

## 6. HTTP/MCP 接口基线

### 客户端接入面

| 路径 | 方法 | 作用 | 默认授权 |
|---|---|---|---|
| `/mcp` | MCP Streamable HTTP 双版本传输所需方法 | 透明代理新式 POST 及旧式 initialize、notifications、tools/list、tools/call、GET/SSE、DELETE 和会话行为 | Gateway Bearer Key |
| `/api/context7/v3/search` | `GET` | 代理官方文档 Search | Gateway Bearer Key |
| `/api/context7/v3/fetch` | `GET` | 代理官方文档 Fetch | Gateway Bearer Key |
| `/api/context7/v2/libs/search` | `GET` | 代理库搜索 | Gateway Bearer Key |
| `/api/context7/v2/context` | `GET` | 代理文档上下文查询 | Gateway Bearer Key |
| `/api/context7/v2/usage` | `GET` | 仅由后台采集 Context7 usage；不向普通 Gateway Key 暴露池级或 Teamspace 级上游 usage | 管理员控制台会话 |
| `/health/live`、`/health/ready` | `GET` | 存活检查和数据库/上游配置就绪情况 | 不含敏感信息 |

网关在 API 门面上不把自身伪装成 Context7 的官方主机。成功 MCP/REST 响应尽可能原样透传；请求关联号通过 `X-Gateway-Request-Id` 响应头提供。仅网关自身产生的错误 envelope 可包含 `gateway_request_id` 字段。

### 用户控制台 API

- `GET /api/user/me`：当前账户、状态及本地计划摘要。
- `GET/POST /api/user/tokens`、`POST /api/user/tokens/:id/rotate`、`DELETE /api/user/tokens/:id`：自助签发、一次性展示、轮换及撤销用户 Token。
- `GET /api/user/usage/summary`、`GET /api/user/usage/events`：本地用量聚合与分页请求记录。
- `GET /api/user/service-status`：网关整体可用性与用户自身 Token 状态；不得泄露池内 Key 数量、Key Hint、其他用户或 Teamspace usage。
- `GET /api/user/setup-guides`：返回基于部署公开 Origin、客户端、选中 Gateway Token 前缀生成的接入片段；Token 值用环境变量占位符表示。
- `POST /api/user/logout`：撤销登录会话。

### 管理员 API

- `GET/POST /api/admin/pools` 与 `GET/POST /api/admin/pools/:id/keys`：建立授权池、录入/停用/恢复/删除密钥、验证凭证。
- `GET /api/admin/keys/:id/status`：检查单 Key 上游可用性和可用的官方 usage 信息；所有返回都必须脱敏。
- `GET /api/admin/users`、`GET/PATCH /api/admin/users/:id`、`PATCH /api/admin/users/:id/quota`：管理用户、状态与本地配额。
- `GET /api/admin/pools/:pool_id/keys/:id/status` 与 `GET /api/admin/pools/:pool_id/keys/:id/usage`：读取池内单 Key 状态/可用上游用量；仅管理员可访问，usage 响应注明所属授权账户/Teamspace 与抓取时间。
- `GET /api/admin/usage/summary`、`GET /api/admin/logs`：管理员运营汇总和过滤请求事件。
- `GET/PATCH /api/admin/settings`、`GET/POST /api/admin/announcements`：系统设置、公告及可审计的修改。
- Phase 6 增加订单/报价/账本的管理员查询，但禁止直接编辑已结算记录；更正通过反向账本事件完成。

## 7. 调度、计量与故障处理规则

### 上游 Key 池

1. 池是管理员声明的授权/账务边界；每个 Key 只能加入一个有效池。新增时验证格式和连通性，验证失败的 Key 默认不能承载业务请求。
2. 第一版采用**池内最近最少使用优先（LRU）**，再以随机/轮转打破同一时间并列；只有状态为 active、未处于限流/熔断期且账户/池策略允许的 Key 才可选择。
3. 不跨池自动回退，不把个人计划/Teamspace 不同的 Key 混成同一个无差别池；管理员显式确认池边界及可用策略后才可加入。
4. `401` 将凭证标为 `invalid` 并停止调度；`403` 标为 `permission_error`，不得将明确的权限/套餐拒绝误判为 Key 失效或切换到另一把 Key 来绕过；`429` 保存 `Retry-After`/限流窗口并遵循上游限制。
5. 只有在请求确定未送达上游时才可重新选 Key 或重试连接；任何请求一旦发送到上游，MCP `tools/call` 均不得自动重放。REST 查询也默认不自动重试，除非 Context7 对该操作明确保证幂等且原请求确定未被处理。`5xx` 通过熔断保护后续流量，不以重放当前调用恢复。
6. 对 `2025-11-25` 旧式传输，若上游返回 `MCP-Session-Id`，网关须为客户端生成不透明 ID，并绑定所属 Gateway Token、原上游 Key 和原上游 session ID；后续 POST、GET/SSE resume 与 DELETE 从此映射选择原 Key。对 `2026-07-28` 新式传输，不合成 initialize/session。对两种版本均须验证每次请求的 Gateway Key、保留协议元数据、透传 SSE/Last-Event-ID 语义，并在客户端断开时取消对应上游流；不得把 SSE 恢复误当作重放一次工具调用。

### 本地额度

- 本地统计维度：请求数、成功/失败数、操作类型、用户/Token、小时/自然日/UTC 月窗口。
- 默认本地业务单位为一条**成功返回的业务调用**计 1 个单位；认证失败、网关健康检查、MCP 传输协商/通知/工具发现不计业务额度；配额策略可按操作配置权重。每个已接受的 MCP 工具调用只创建一个 usage event；后续为同一响应流服务的 SSE resume、GET 或 session 恢复不再计一次。
- 本地频率限制和本地小时/日/月预算独立实施：所有已发往上游的请求（包括失败）计入请求速率；失败调用不扣本地业务单位。管理员可通过新版本计划调整后续规则，不回写旧记录。
- 上游 usage API 数据按官方返回原样归档为上游快照，注明查询时间、所属 Key/池和返回字段；不基于此推测或承诺未来账单金额。
- 配额并发检查必须原子化；用户用量达到上限时在发往上游前拒绝，并给出可理解的本地配额错误。

### 错误与可观测性

- 每个请求生成 `request_id`，在 UI、响应的安全元数据和服务器日志中关联；不要记录 Authorization、Cookie、上游 Key、查询原文或文档正文。
- 上游错误尽可能透传原始状态与 MCP 错误代码；网关错误分类至少包括 `invalid_gateway_key`、`token_disabled`、`quota_exceeded`、`pool_unavailable`、`upstream_auth_error`、`upstream_rate_limited`、`upstream_unavailable`。
- 日志保留默认 30 天（可配置但必须有上限）；按周期清除详细事件、保留必要聚合账务记录。提供日志导出应自动脱敏。
- 基本指标：请求量、p50/p95/p99 延迟、按操作/状态的成功失败数、上游 401/403/429/5xx、活跃/不可用 Key 数、用户本地额度拒绝数、DB/迁移错误。

## 8. 分阶段实施与验收

### 实施进度（2026-11-22；以可复跑检查为准）

| 阶段 | 当前状态 | 证据 / 未完成事项 |
|---|---|---|
| Phase 0 — 上游准入 | **部分完成 / Go-No-Go 未解除** | 官方文档已查阅；2026-11-22 本地成功冒烟覆盖 MCP 新旧协议与四个 REST 读接口；401/403/429/5xx、legacy session 绑定、SSE 恢复和去重计费已由 loopback mock 测试覆盖。运营者须另行记录 Context7 对目标用户范围和 Key 池的授权，并在部署计划上做一次受控负向验证。 |
| Phase 1 — 骨架与持久化 | **实现完成** | Rust/Axum、React/Vite、SQLx SQLite 迁移、健康检查、CI、Compose 和数据库备份/恢复脚本已提供。需在目标部署主机再运行一次镜像构建/恢复演练。 |
| Phase 2 — 身份与密钥池 | **实现完成；外部 OAuth 配置待部署验证** | Linux.do state+PKCE、浏览器会话、Passkey 管理员及初始/恢复注册码、上游密钥加密、Gateway Key scope/expiry/rotate/revoke 已实现；Passkey 有软件认证器端到端测试。 |
| Phase 3 — MCP/REST 代理 | **实现与 mock 验收完成；真实 session 仍待部署观测** | 2026-11-22 经本地 Gateway 的 MCP `tools/list`/两个标准工具和四条 REST 均成功；mock 覆盖 401/403/429/5xx、熔断、session 跨 Token 隔离和 SSE 恢复不重复扣费。上游未下发真实 session，部署后应再观测一次 session 生命周期。 |
| Phase 4 — 配额与用量 | **实现完成** | 原子预留/结算、并发与边界测试、请求事件、配额汇总、保留期和 60 秒清理已接入。 |
| Phase 5 — 控制台 | **实现完成** | 中英文用户/管理员界面和设置指南已接入；Playwright 流程 11/11 通过，`npm run build` 通过。 |
| Phase 6 — 本地订单/额度 | **本地流程完成；真实支付未纳入** | 本地积分账本、兑换码、签名沙箱订单、反向调账和对账 API/UI 已实现；生产 payment provider 故意禁用，订单额度是 Gateway 本地额度。 |
| Phase 7 — 发布加固 | **实现与容器验收完成；审计/演练待外部执行** | 非 root、只读 rootfs、cap-drop、no-new-privileges 的生产 Compose 已实测健康；镜像构建成功；SQLite 备份/恢复演练、`npm audit`（生产依赖 0 漏洞）与 Compose 配置检查通过。独立安全评审、生产负载与部署特定恢复演练仍未完成。 |

阶段“实现完成”仅表示本仓库内交付和现有测试达到标记，不等于 Phase 0 授权 Gate 已通过，也不构成生产上线批准。

### Phase 0 — 产品与上游准入验证（上线阻断 Gate）

**目标**：在实现密钥池前先确认授权和上游契约，避免把不允许的密钥共享/限额规避写进产品。

**工作项**
- 记录拟服务用户范围、上游 Key 来源、账户/Teamspace 归属和允许的代理/共享边界；取得可审计的授权依据。
- 在开发环境通过 Context7 官方接口验证：MCP 2026 新式 POST 元数据和 legacy initialize/session 两种传输（tools/list、resolve-library-id、query-docs）、REST Search/Fetch、API Key 鉴权、usage 返回、`401/403/429` 行为与限流头。
- 记录需要支持的 Context7 官方接口版本、实际配额观察来源和 Key 生命周期；不复制官方 Key UI/账户管理。
- 确认目标公开域名、TLS/反向代理、部署目标及 OAuth 回调域名。

**交付物**：授权/账户范围登记、经验证的上游契约样例（脱敏）、错误/限流行为表、产品 Go/No-Go 记录。记录模板见 [`phase0-authorization-record.md`](phase0-authorization-record.md)；未签字前保持注册关闭。

**验收门槛**：关键 MCP 与 REST 操作可重复验证；运营者明确确认密钥池用途和用户授权范围。未通过后者时只允许本地 mock 或单账户试验，不进入开放多用户上线。

### Phase 1 — 工程骨架、配置与持久化

**目标**：构建可重复开发、测试、部署的空壳应用与安全默认值。

**工作项**
- 初始化 Git、Rust workspace/backend、React/Vite frontend、数据库迁移、统一格式化/静态检查和 CI。
- 统一配置加载与校验：公开 Origin、上游固定 Origin、监听地址、数据库路径、密钥加密密钥、Cookie 安全选项、日志保留、认证适配器；生产配置错误时拒绝启动。
- 实现 `/health/live`、`/health/ready`、版本信息、结构化请求 ID、数据库迁移/备份脚本与本地 mock Context7。
- 建立 RBAC 与 middleware 骨架、CSRF/CORS/CSP 策略、错误统一类型和测试 fixtures。

**交付物**：可运行的 backend/web skeleton、迁移基线、CI 管道、Dockerfile/Compose 开发配置、`.env.example`（不含真实密钥）。

**验收标准**：新克隆环境按文档启动；迁移从空 DB 成功；错误配置在启动时报可理解错误；格式化、lint、Rust/前端单测、构建全通过；mock 上游可覆盖成功/失败/SSE。

### Phase 2 — 身份、Gateway Key 与上游 Key 池管理

**目标**：先实现密钥边界和管理权限，再开放代理流量。

**工作项**
- 用户：接入 Linux.do Connect OAuth 登录和登出、会话轮换、账户禁用；OAuth 回调校验 state/nonce、过期与重复回调。
- 管理员：独立 Passkey/WebAuthn 首次注册与恢复流程；确保用户会话不能调用管理员接口。
- Gateway Key：签发高熵随机 Token、一次展示、摘要校验、命名、撤销、轮换、过期和权限 scope；同一用户可有多把 Key。
- 上游池：管理员创建授权 Pool、录入 Context7 Key、应用层加密、验证、停用/恢复、删除、状态检查与失败原因；列表/详情仅展示脱敏值。
- 为 Key 增加显式的授权账户/Teamspace 标签与策略；每个 Key 的归属和加入操作记录审计事件。

**交付物**：用户/管理员认证、`users`/`gateway_tokens`/`upstream_pools`/`upstream_keys`/审计迁移、管理员 Key 管理 API 与最小操作页。

**验收标准**：Gateway Key 只显示一次且 DB 中找不到明文；撤销后立刻无法鉴权；普通用户无法列举/验证/停用上游 Key；DB 被复制但没有应用主密钥时不能解密上游 Key；跨用户、跨管理员权限和 OAuth CSRF 测试全通过。

### Phase 3 — MCP/REST 转发与受控调度

**目标**：实现核心业务闭环：用户凭 Gateway Key 调 Context7，网关安全选择一把获准上游 Key。

**工作项**
- MCP `/mcp`：兼容已验证的 MCP 2026 新式 Streamable HTTP 与 legacy 2025-11-25 传输；代理协议元数据、SSE、Last-Event-ID 和 legacy session 标识；原样承载 JSON-RPC body；不缓冲大流式响应到内存。
- REST 门面：实现 Phase 0 验证过的只读 Search/Fetch/Library Search/Context/Usage 路由；严格 allowlist path、method、query 参数和允许转发的 headers。
- 在请求进入业务调度前完成 Gateway Key、用户状态、scope 和基础频率限制检查；上游只接受系统固定 Origin。
- 实现池内 eligible Key 选择（LRU），池状态失效、上游 401/403、429、5xx 的状态转换、冷却/熔断及响应传递；不在上游已接收请求后自动换 Key 重放。
- 为 REST 与 MCP 使用同一 credential resolver、调度服务、限流处理和 request ID，不建立两套不一致的访问控制。

**交付物**：完整 MCP proxy、只读 REST proxy、上游 Key selection service、合约测试集及命令行 smoke test。

**验收标准**：新式 MCP 客户端可完成请求协商并列工具/调用两个标准工具；legacy 客户端可完成 initialize、列工具与调用；同一 Token 的 REST/MCP 请求均只使用授权池；客户端不能覆写上游地址或 Authorization；SSE/恢复行为不损坏；401/403/429/5xx 的策略正确且无隐式越限 fallback。

### Phase 4 — 配额、计量、请求历史与管理员运营 API

**目标**：让运营者能控制本地服务额度并解释每次请求的结果。

**工作项**
- 建立本地配额计划、用户分配、小时请求频率与小时/日/月预算；实现 UTC 月与自然日窗口定义、原子并发计数和计划变更版本化。
- 记录最小化 `usage_events`，建立用户与管理员汇总、按操作/状态/时间过滤及分页接口；用户永远只看到自己的事件。
- 管理员获取 Context7 官方可用 usage 快照；对 403/404/不支持状态优雅降级，不伪造数值。
- 增加池可用性、单 Key 状态、慢请求、失败/限流率、配额拒绝等系统状态和排障事件。
- 实现日志清理任务、管理员审计事件和脱敏导出。

**交付物**：配额/usage migration、配额 API、用户/管理员 usage API、后台清理任务、观测指标与脱敏日志策略。

**验收标准**：边界时间和并发压力测试不能超发本地额度；MCP 元数据请求不扣业务额度；业务调用的记数规则一致；日志无密钥/Authorization/原始 prompt/文档正文；用户 API 不能越权查询他人数据；上游用量快照清楚标记其数据来源与抓取时间。

### Phase 5 — 用户与管理员控制台、接入指南

**目标**：交付与 Hikari 同类的完整日常操作体验。

**工作项**
- 用户控制台：账户概览、当前套餐/剩余额度、小时/日/月用量图表、Gateway Key 创建/复制/撤销、Token 活跃状态、请求历史与错误详情。
- 使用方法页：选中客户端后生成 MCP/HTTP 配置步骤、可复制代码、环境变量配置方式、连接验证和排错提示；展示的是 Gateway Origin 与 Gateway Token，不包含 Context7 上游 Key。
- 管理员控制台：系统健康、上游 Pool/Key CRUD 与状态、用户查询/禁用、配额调整、用量/请求日志过滤、公告和系统设置。
- 为窄屏实现可操作布局；空状态、加载、权限拒绝、部分上游不可用、无可用 Key、额度耗尽均有明确状态。
- 将 UI 文案与路由拆分为本地化资源；初始支持中文与英文。

**交付物**：可供用户与管理员实际使用的 web console、Codex/Claude/Cursor/VS Code/Windsurf/通用 MCP 与 REST 指南、截图/Storybook 或等价交互测试。

**验收标准**：端到端测试涵盖登录、首次创建 Gateway Key、配置客户端、MCP 调用、超额提示、撤销 Key、管理员禁用上游 Key、检查请求记录；所有敏感值仅在正确的创建/输入环节出现；键盘与移动端主流程可完成。

### Phase 6 — 本地额度充值、报价和订单（可选；不影响核心网关）

**目标**：按 Hikari 功能提供可选商业化能力，同时与 Context7 的上游账单保持分离。

**工作项**
- 实现本地产品报价和订单预览：额度档位、有效期、适用窗口、币种金额、最终额度变化均由服务端计算并返回。
- 新增可插拔支付 provider，首个适配器可为 Linux.do Credit；无商户配置时不显示充值入口并拒绝订单创建。
- 创建支付前保存待支付订单；回调验签、过期检查、金额/商户匹配、订单唯一性、幂等结算；失败/取消不加额度。
- 使用不可变 quota ledger 记录授予/消费/退款/人工调整，不以修改已结算订单来更改历史。
- UI 展示订单状态、充值预览、历史订单和到账后的额度变化。

**交付物**：订单/报价 API、支付 provider interface 与可选实现、额度 ledger、回调安全测试、用户充值页及运维对账说明。

**验收标准**：重复/乱序/伪造回调不产生重复入账；金额及币种由服务端核验；支付金额与 Gateway 本地额度转换可审计；界面和文案明确该额度不属于 Context7 官方计划/额度。

### Phase 7 — 生产加固、发布和运维闭环

**目标**：达到可安全部署、升级、备份、观测和回滚的运营标准。

**工作项**
- 审核 TLS、反向代理信任边界、Secure/HttpOnly/SameSite Cookie、CSP、CSRF、CORS、速率限制、登录暴力尝试和管理员恢复路径。
- 容器以非 root 用户运行，最小化文件权限，健康检查不过度暴露内部信息；锁定依赖并进行依赖漏洞扫描与 SBOM 输出。
- 设计 SQLite 在线备份/定期恢复演练、迁移前备份、版本回滚约束、保留策略和事故响应手册。
- 完成集成/端到端测试、关键组件故障演练、负载/并发测试、恢复测试与第三方安全评审。
- 配置告警阈值：池中可用 Key 低水位、连续 401/403/429、上游延迟/失败激增、磁盘/DB 错误、支付回调异常、管理员登录异常。

**交付物**：生产 Docker Compose/部署说明、备份与恢复 SOP、威胁模型、告警面板/日志规范、版本发布与回滚清单。

**验收标准**：从空环境可完成部署并验证健康状态；恢复演练成功；密钥轮换演练可不中断或按预期 fail closed；安全/负载/恢复检查通过；Phase 0 的授权 Gate 仍有效且适用于当前用户范围。

## 9. 全局测试矩阵

1. **上游合约**：MCP 2026 新式与 2025 legacy initialize/tools/list/tools/call、会话、SSE/事件恢复，REST 参数与响应、认证、限流头、上游超时和不规范响应。
2. **安全隔离**：Gateway Key 随机性/摘要保存/撤销、用户间越权、管理员/用户权限边界、上游 Key 密文、日志脱敏、任意 URL/路径/Header 注入。
3. **配额/调度**：UTC 月界、日界、并发计数、禁用/失效/限流 Key、无 Key 可用、池边界、`Retry-After`、禁止以 429 触发违规密钥轮换。
4. **前端流程**：登录、Token 新建与轮换、教程代码、MCP 连通验证、额度不足、日志分页、公告、响应式。
5. **支付（Phase 6）**：订单状态机、签名验证、重放/幂等、金额不匹配、过期、失败回调、额度账本反向调整。
6. **运行与恢复**：从零安装、升级迁移、备份、恢复、错误密钥加密密钥、上游不可达、DB 只读/满盘、容器重启。

## 10. 文档、配置与运维交付清单

- `README.md`：产品用途、功能范围、启动方式、部署入口和链接到专题文档。
- `docs/IMPLEMENTATION_PLAN.md`：本实施基线。
- `docs/architecture.md`：组件图、请求/鉴权/调度/数据流、信任边界。
- `docs/context7-upstream-contract.md`：Phase 0 验证后的接口样例、参数/状态/错误/限流行为，样例全部脱敏。
- `docs/security.md`：威胁模型、Key 加密、用户/管理员权限、日志和应急轮换。
- `docs/deployment.md`：Docker、TLS/代理、环境变量、迁移、备份恢复、升级回滚。
- `docs/operations.md`：上游池维护、用户/额度管理、限流故障排查、日志/告警和事故处理。
- `docs/client-setup.md`：各 MCP 客户端和 HTTP API 接入指南。
- `.env.example`：仅示例变量名与占位符，不得提交真实凭证。

## 11. 假设与明确暂缓项

- 假设项目目标是由单一运营者面向获准用户提供服务；若 Context7 授权范围限定为组织内网/Teamspace，用户注册必须限制到相应身份/组织。
- 上游 Key 池支持多把凭证，但不会自动将不同许可、账户或 Teamspace 混成跨租户共享池。
- Linux.do Connect 是初始用户登录适配器；如果部署目标不能使用它，应新增 OAuth/OIDC adapter，而不是把 OAuth 逻辑耦合到网关业务代码。
- 首期仅单节点 SQLite；没有容量与可用性需求前不做 HA、分布式 session 或多节点配额锁。
- Context7 Admin MCP、上游 Key 自动购买、官方计划/套餐销售、共享 Context7 账户 UI、文档缓存/索引、自定义上游代理、任意 URL 转发均不在 v1 范围。
- 支付阶段可以整体关闭，不阻塞 MCP/REST 网关、账户、Key 池和用量功能交付。

## 12. 官方参考资料

- [Context7 文档目录](https://context7.com/docs/llms.txt)
- [Context7 MCP 安装指南](https://context7.com/docs/installation)
- [Context7 API Guide](https://context7.com/docs/api-guide)
- [Context7 MCP Server README](https://github.com/upstash/context7/blob/master/packages/mcp/README.md)
- [Context7 API Key 指南](https://context7.com/docs/howto/api-keys)
- [Context7 API Usage Reference](https://context7.com/docs/api-reference/usage/get-api-usage)
- [Context7 服务附加条款](https://upstash.com/trust/context7addendum.pdf)
- [Tavily Hikari README](https://github.com/IvanLi-CN/tavily-hikari/blob/main/README.md)
- [Tavily Hikari 产品定义](https://github.com/IvanLi-CN/tavily-hikari/blob/main/PRODUCT.md)
- [Hikari 配额设计](https://github.com/IvanLi-CN/tavily-hikari/blob/main/docs/quota-design.md)
- [Hikari HTTP API Proxy 设计](https://github.com/IvanLi-CN/tavily-hikari/blob/main/docs/tavily-http-api-proxy.md)
- [Hikari 高匿名代理设计](https://github.com/IvanLi-CN/tavily-hikari/blob/main/docs/high-anonymity-proxy.md)
