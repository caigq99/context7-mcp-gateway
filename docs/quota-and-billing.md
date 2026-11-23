# 本地配额、用量和订单

## 配额配置

在管理控制台为获准的账户分配额度。未分配计划的账户无法调用上游。`GET /api/admin/users/{user_id}/quota` 返回当前版本和用量。`PATCH` 接受 `requests_per_five_minutes`、`business_calls_per_hour`、`daily_units`、`monthly_units` 和可选 `weights`。`expected_version` 可用于防止并行编辑覆盖。首次分配时预期版本为 `0`。新版本应用于随后发出的请求，已预留的请求按受理时的权重结算。UTC 当前窗口内的已用量会延续。

Linux.do 新用户会按 OAuth `/api/user` 返回的 `trust_level` 自动获得初始配额。等级缺失或越界时不创建默认计划，也不会自动删除已有计划。管理员仍可通过用户详情中的 **Local quota** 覆盖自动计划。

| Linux.do 等级 | 每 5 分钟请求 | 每小时业务调用 | UTC 每日积分 | UTC 每月积分 |
| --- | ---: | ---: | ---: | ---: |
| L0 | 10 | 30 | 10 | 50 |
| L1 | 30 | 60 | 20 | 300 |
| L2 | 60 | 240 | 70 | 1,000 |
| L3 | 120 | 600 | 200 | 3,000 |
| L4 | 180 | 900 | 400 | 6,000 |

等级在后续登录时变化时，仅当当前计划仍逐项等于旧等级默认值且权重为 `{}` 时才自动切换；管理员手工调整过的计划不会被登录流程覆盖。升级立即写入新计划版本，降级同样在下次登录时识别，但不会追溯修改已结算用量。

| 约束 | 计算方式 |
| --- | --- |
| 五分钟频率 | 同一账户所有 Gateway Key 共用滚动窗口。网关接纳的向上游发送尝试计 1 次。 |
| 小时业务次数 | UTC 自然小时内的业务发送尝试，包含上游失败。 |
| 日与月用量 | 以受理时所在的 UTC 自然日和月份为准，每条成功业务调用默认计 1 个 Gateway 单位。两项上限分别检查。 |
| 操作权重 | 可为 MCP `tools/call` 及各 REST 操作设置 1 至 1000 个单位。 |

SQLite 在 `BEGIN IMMEDIATE` 事务中检查并持久化预留。MCP 工具发现、协商、通知及 SSE 恢复占用请求频率，业务单位为零。MCP 工具调用以对应的 JSON-RPC 结果结算，JSON-RPC 错误与工具级错误释放预留。REST 业务调用按成功响应完成结算。网关在发送上游前拒绝的请求（例如 Key 租约获取失败）同时退还请求频率与业务预留；取消、上游失败和超过五分钟的未结算预留释放业务额度。可恢复的 SSE 流在原预留有效期末之前可继续结算。达到本地限制会返回 `429` 和 `quota_exceeded`，有已知窗口时提供 `Retry-After`。

## 用量和日志

`/api/user/usage/summary` 返回当前配额窗口、本 UTC 月总计、近 7 个 UTC 日期的请求和单位聚合，以及本月已受理请求（仍在详细记录保留期内）的 p50、p95、p99 端到端延迟。`/api/user/usage/events` 仅检索当前账户的记录。管理端通过 `/api/admin/usage/summary`、`/api/admin/logs` 和 `/api/admin/logs/export` 获取全局统计、分页记录与 NDJSON 下载。事件按配置的保留期每天清理；失败/取消/过期预留每 60 秒分批回收，聚合账本保留。

事件查询支持 `operation`、`status`（`success`、`failure` 或 HTTP 状态码）、`from`/`to`（UTC Unix 秒）、`token_id`、`request_id`、`limit`、`cursor`。管理员可额外用 `user_id` 过滤。下载最多返回 1000 条记录，响应头 `X-Next-Cursor` 可用于获取下一页。日志记录操作类型、时间、状态和关联 ID，用户响应隐藏池与上游密钥 ID。详细记录默认保留 30 天，清理后仍保留计量聚合。

## 订单与兑换

充值增加 Gateway 本地月额度。五分钟频率、小时次数、日额度和基础计划分配仍需满足。基础月额度优先使用，超出部分从最早到期的额度批次预留。成功时写入消费账本，失败时释放批次额度。

订单默认关闭。开发或测试环境可通过 `GATEWAY_BILLING_SANDBOX_SECRET` 配置沙箱，值为独立的 32 字节 base64url 密钥（无填充）。沙箱使用虚拟币种 `TST`，适用于本地签名和订单生命周期测试，生产环境自动禁用。

1. `GET /api/user/billing` 返回沙箱开关、报价和可用补充额度。`POST /api/user/orders/preview` 用 `quote_id` 与 `quote_version` 计算预览。
2. `POST /api/user/orders` 保存待支付订单及服务端确认的币种、金额和额度，待支付订单 30 分钟后到期。`GET /api/user/orders` 查询当前账户订单。通过 `/api/user/orders/{order_id}/cancel` 可取消待支付状态。
3. `POST /api/billing/callback/sandbox` 接收完整签名事件。`X-Gateway-Billing-Signature` 为 `base64url(HMAC-SHA256(secret, b"context7-gateway.billing-sandbox.v1\0" || raw_body))`。事件包含 UUID `event_id`、UUID `order_id`、`merchant_id: "local-sandbox"`、`amount_minor`、`currency: "TST"`、`status`（`paid`、`failed`、`cancelled`）。验签后校验原订单的金额、币种、商户、过期时间和状态。订单变更、额度授予及事件登记在同一个事务中完成。
4. `POST /api/admin/billing/codes` 接受 `units`、`valid_days` 和 `expires_in_days`，返回一次性兑换码。用户通过 `POST /api/user/billing/redeem` 提交 `code`。同一兑换码只授予一次，额度从兑换时起计有效期，兑换码原文在运营者签发时展示一次。
5. `/api/admin/billing/adjustments` 接受 `{user_id,units,valid_days,idempotency_key,reason}`：正数授予额度，`valid_days=0` 表示不失效；负数从最早到期的可用批次扣减且不能超过未预留余额。调整键必须唯一，完全相同的重试不会再次改账，不同内容复用同一键会冲突。账本保留 `manual` 符号变动，管理员审计日志保留执行者、理由和关联请求号。`/api/admin/orders` 和 `/api/admin/billing/ledger` 提供对账。未消费且无活动预留的订单可以通过 `POST /api/admin/orders/{order_id}/reverse` 追加反向账本记录。支付和额度授予记录继续保留。沙箱反向调整只影响本地额度。
