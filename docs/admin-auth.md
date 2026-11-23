# 管理员登录、通行密钥与恢复

生产管理接口使用独立的管理员认证和会话，不经过 Linux.do 用户登录，也不复用用户 Cookie。默认引导使用 `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD` 密码登录；通行密钥（Passkey）是登录后可选绑定的附加凭证，密码登录始终保留。只有两个密码变量都未配置时，才使用兼容的 passkey-only 模式。

## 默认引导：用户名与密码

1. 在部署主机的 `.env` 或等价 Secret 管理器中同时设置以下两项：

   ```sh
   GATEWAY_ADMIN_USERNAME=...
   GATEWAY_ADMIN_PASSWORD=...
   ```

   两项必须成对配置；只设置一项会在启动时报配置错误。两项都留空时进入 passkey-only 兼容模式（见下文）。`.env` 必须为 `chmod 600`，不要提交到 Git，也不要贴到聊天或工单。

   启动校验要求用户名为 1–64 个字符且不含控制字符；生产环境密码至少 12 个字符、最长 256 个字符，且不含控制字符。建议使用密码管理器生成 20 个以上随机字符，并确保不与任何其他账户复用。

2. 启动 Gateway，在浏览器打开 `https://<hostname>/admin/login`；本地预览使用 `http://localhost:8787/admin/login`。配置了密码时，登录页默认显示用户名/密码表单。

3. 输入 `GATEWAY_ADMIN_USERNAME` 和 `GATEWAY_ADMIN_PASSWORD` 登录。密码只从部署环境读取；进程启动时以 Argon2id 在内存中生成哈希，明文不写入数据库、日志、前端或审计记录。

4. 首次登录后即可管理 Context7 上游 Key 池、用户、Gateway Key 和设置。进入 **Settings -> Passkeys** 可以绑定可选的 Passkey；绑定后密码登录仍然可用，不需要先完成 CLI 注册。

`GATEWAY_ADMIN_TOKEN` 仍仅用于开发/测试脚本的固定 Bearer 认证；生产配置拒绝非空值，不能替代生产密码登录或 Passkey。

## 登录方式与状态

- 配置了密码时，`/admin/login` 默认展示用户名/密码表单。
- 至少存在一把有效 Passkey 时，页面额外显示可选的 **Sign in with passkey** 按钮；管理员可以继续选择密码登录。
- `passkey_available` 表示当前 Origin/协议支持 WebAuthn 典礼；`passkey_login_available` 表示数据库中至少有一把有效 Passkey。浏览器还需要安全上下文和 WebAuthn 支持，前端才会实际显示 Passkey 按钮。
- 未配置密码（两项都留空）时，登录页保留 passkey-only 流程：已有 Passkey 可登录；没有任何 Passkey 时可用一次性注册码完成初始绑定。

## 密码轮换与会话失效

密码轮换通过更新部署环境的 `GATEWAY_ADMIN_PASSWORD` 并重启 Gateway 完成：

```sh
chmod 600 .env
# 编辑 .env 中的 GATEWAY_ADMIN_PASSWORD，保存后重启 Gateway。
docker compose up -d --force-recreate context7-gateway
```

已通过密码建立的会话绑定当前管理员用户名/密码配置的会话版本。修改密码（或用户名）并重启后，旧密码登录会话立即失效；Passkey 会话不会仅因密码轮换而失效，如怀疑凭证泄露应同时在 **Settings -> Passkeys** 移除相关 Passkey。密码轮换本身不需要数据库迁移，也不会把新密码写入数据库。

如果密码遗失，只有具备部署主机或 Secret 管理权限的运营者才能恢复：在 `.env`/Secret 管理器中设置新的强密码并重启 Gateway，然后用新密码登录。建议离线备份 `.env` 或使用平台的 Secret 版本管理；不要把密码写入镜像、Compose 文件、脚本、工单或审计备注。

## 使用 Passkey 登录（可选）

绑定 Passkey 后，已注册凭证可以在登录页的 **Sign in with passkey** 入口完成认证。Passkey 是可选的第二登录方式，不会替代或禁用密码登录：

- 密码登录启用时，可以在 **Settings -> Passkeys** 添加多把 Passkey。
- 移除某把 Passkey 会撤销由该 Passkey 创建的管理员会话；密码登录启用时，即使移除最后一把 Passkey 也仍可用密码登录。
- 仅 passkey-only 模式（密码未配置）继续保留“最后一把 Passkey 不能移除”的保护，`DELETE /api/admin/auth/passkeys/{id}` 在移除最后一把时返回 `409`。
- 更换站点域名会改变 WebAuthn 依赖方 ID。密码登录不受该变化影响；Passkey 需要在新域名下重新绑定或通过 passkey-only 恢复流程处理。

## 仅 Passkey 模式：CLI 初次注册与恢复（旧版兼容）

本节只适用于明确不设置 `GATEWAY_ADMIN_USERNAME` 和 `GATEWAY_ADMIN_PASSWORD` 的部署，或用于迁移前的旧环境。配置了密码的部署不要把 CLI 注册码作为默认引导路径；直接使用密码登录，并在 **Settings -> Passkeys** 中绑定 Passkey。

### 初次注册

1. 将 `PUBLIC_ORIGIN` 配置成浏览器使用的 HTTPS Origin，例如 `https://gateway.example.com`。使用固定的 DNS 主机名并保持端口一致。开发环境可使用 `http://localhost:8787` 测试 WebAuthn，浏览器地址也应使用 `localhost`。
2. 启动 Gateway 后，在有权访问同一持久化数据库的终端执行以下命令。标准输出为有效期 15 分钟的单次注册码，重新签发会替换上一张注册码。

   ```sh
   docker compose exec context7-gateway context7-gateway admin issue-enrollment
   ```

3. 打开 `/admin/login`，选择注册 Passkey，粘贴注册码并填写可辨认的名称，在浏览器提示中验证指纹、PIN 或其他凭证解锁方式。
4. 在 **Settings -> Passkeys** 添加另一把可独立使用的 Passkey。移除设备会终止由该 Passkey 创建的会话；passkey-only 模式会保留至少一把有效凭证。

### 恢复访问

已有 Passkey 仍可使用时，登录后从管理页添加替代设备，确认替代设备可以登录后再移除旧设备。所有设备不可用时，由具备 Gateway 主机权限的运营者运行恢复命令：

```sh
docker compose exec context7-gateway context7-gateway admin issue-enrollment --recovery
```

在管理员登录页使用新注册码完成注册。旧 Passkey 和已登录浏览器在新凭证验证成功时一次性失效；恢复未完成或注册码过期时，已注册凭证继续可用。恢复后的管理员应重新添加备份设备。

从 passkey-only 模式切换到密码模式时，设置两个管理员密码变量并重启；已有 Passkey 保留，密码登录立即生效，之后不再需要 CLI 注册码。反向切回 passkey-only 模式前，请确认至少有一把可用 Passkey。

## HTTP 接口

以下路由位于 `/api/admin/auth` 下。浏览器写请求必须发送与 `PUBLIC_ORIGIN` 一致的 `Origin`。完成 Passkey 请求发送 `{ "credential": ... }`，其中 `credential` 为 WebAuthn API 产生的凭证 JSON；启动 Passkey 请求返回浏览器需要的 `{ "publicKey": ... }`。认证接口响应使用 `Cache-Control: no-store`。

| 请求 | 功能 | 成功返回 |
| --- | --- | --- |
| `GET /session` | 查询当前管理员会话及可用登录方式 | `{ "authenticated": true, "password_login_available": true, "passkey_available": true, "passkey_login_available": true }` 或相应布尔值 |
| `POST /password/login` | 接收 `{ "username": "...", "password": "..." }` 并建立管理员会话 | `200`，会话状态和管理员会话 Cookie。错误凭据返回 `401 invalid_admin_credentials`；重复失败触发限流后返回 `429` |
| `POST /login/start` | 为现有 Passkey 生成认证挑战 | `200`，浏览器可直接消费的 WebAuthn request options 和挑战 Cookie |
| `POST /login/finish` | 验证签名并轮换当前会话 | `200`，会话状态和管理员会话 Cookie |
| `POST /register/start` | 接收 `{ "name": "..." }`，在已登录的管理员会话中绑定新 Passkey；passkey-only 模式可改用 `{ "name": "...", "enrollment_code": "..." }` | `200`，浏览器可直接消费的 WebAuthn creation options 和挑战 Cookie |
| `POST /register/finish` | 验证并记录新 Passkey | `200`，会话状态和管理员会话 Cookie |
| `POST /logout` | 撤销当前管理员会话和该浏览器未完成的挑战 | `204`，清除管理员 Cookie |
| `GET /passkeys` | 列出有效 Passkey 的公开标识、名称与使用时间 | `200`，`[{ "id": "...", "name": "...", "created_at": 0, "last_used_at": null }]`，时间为 Unix 秒 |
| `DELETE /passkeys/{id}` | 撤销一把 Passkey 与其会话 | `204`；仅 passkey-only 模式尝试移除最后一把时返回 `409` |

错误凭据统一返回 `401 invalid_admin_credentials`，不会区分用户名或密码错误。重复失败由服务端限流，超过阈值后返回 `429`（并带 `Retry-After`）；反向代理层仍应配置独立的登录限流作为纵深防御。未配置密码时调用密码登录返回 `503 not_configured`。Passkey 无效凭证、过期挑战和重复完成返回 `401`；HTTP Origin 不一致返回 `403`；域名或协议的开发环境配置无法使用 Passkey 时返回 `503`。

## 持久化与并发

- `admin_account` 存储管理员的随机用户标识。所有 Passkey 都绑定此身份。
- `admin_password_sessions` 保存密码登录会话令牌的 SHA-256 摘要、绑定当前用户名/密码配置的版本、Origin、八小时绝对有效期和撤销时间。密码或用户名变更并重启后，旧版本密码会话无法通过校验。
- `admin_passkeys` 保存 WebAuthn 公钥凭证、最近使用时间、签名计数和撤销时间。完成登录时，事务再次确认凭证仍有效并检查最新计数，允许不提供计数器的同步 Passkey 使用零计数。
- `admin_ceremonies` 保存最多五分钟的服务端挑战状态，登录和注册使用不同的挑战 Cookie。完成请求通过带有效期过滤的 `DELETE … RETURNING` 取回且消耗挑战，签名验证失败也不能提交同一挑战。每种挑战最多保留 256 条待完成记录。
- `admin_sessions` 保存 Passkey 会话令牌的 SHA-256 摘要、绑定凭证、Origin 与八小时绝对有效期。生产 Cookie 使用 `Secure`、`HttpOnly`、`SameSite=Strict` 和 `__Host-` 前缀。登录或注册成功会撤销浏览器提交的旧管理员会话。
- `admin_enrollment_tickets` 保存 passkey-only 模式注册码的摘要。成功注册时，消耗注册码、插入新凭证、签发会话和写入审计事件在同一事务完成。成功恢复时，该事务同时撤销所有旧管理员凭证、会话和待完成挑战。
- 密码明文和 Argon2id 哈希都不写入数据库。密码登录成功会写入管理员审计事件，但审计事件只记录操作者和结果，不包含用户名、密码或哈希。

## 验收

```sh
cargo test --workspace admin_auth::tests
```

测试应覆盖密码登录成功/失败、重复失败限流、密码轮换后的会话失效、密码会话绑定 Passkey、密码登录下移除最后一把 Passkey，以及 passkey-only 模式的初始注册、挑战并发重放、Cookie 绑定、最后一把 Passkey 保护和恢复后的整体撤销。还应在实际 HTTPS 域名下完成一次密码登录、Passkey 绑定、退出与两种登录方式的回归测试。
