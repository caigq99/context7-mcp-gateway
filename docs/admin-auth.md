# 管理员通行密钥与恢复

生产管理接口使用独立的 WebAuthn 管理员会话。运营者通过本地 CLI 签发注册码，在浏览器绑定通行密钥后管理 Context7 上游 Key 池、用户和 Gateway Key。Linux.do 登录会话和 Gateway Key 均无管理员权限。

## 首次注册

1. 将 `PUBLIC_ORIGIN` 配置成浏览器使用的 HTTPS Origin，例如 `https://gateway.example.com`。使用固定的 DNS 主机名并保持端口一致。在开发环境可使用 `http://localhost:8787` 测试 WebAuthn，浏览器地址也应使用 `localhost`。
2. 启动 Gateway 后在有权访问同一持久化数据库的终端执行 `context7-gateway admin issue-enrollment`。运行中容器可执行以下命令。标准输出为有效期 15 分钟的单次注册码，重新签发会替换上一张注册码。

   ```sh
   docker compose exec context7-gateway context7-gateway admin issue-enrollment
   ```

3. 打开管理员登录页 `/admin/login`，选择注册通行密钥，粘贴注册码，填写可辨认的名称，在浏览器提示中验证指纹、PIN 或其他凭证解锁方式。完成后浏览器获得管理员会话。
4. 在管理员设置 `/admin/settings` 添加另一把可独立使用的通行密钥。移除设备会终止由该 Passkey 创建的会话，系统会保留至少一把有效凭证。

`GATEWAY_ADMIN_TOKEN` 可为开发与测试脚本提供临时管理认证。生产配置中留空或删除这个环境变量，启动时会拒绝配置非空值。

## 恢复访问

已有通行密钥仍可使用时，登录并从管理页添加替代设备，确认替代设备可以登录后再移除旧设备。所有设备不可用时，由具备 Gateway 主机权限的运营者运行恢复命令。

```sh
docker compose exec context7-gateway context7-gateway admin issue-enrollment --recovery
```

在管理员登录页使用新注册码完成注册。旧 Passkey 和已登录浏览器在新凭证验证成功时一次性失效。恢复操作未完成或注册码过期时，已注册的凭证继续可用。恢复后的管理员应重新添加备份设备。所有注册、恢复、登录、注销和移除操作可通过管理员审计记录核查。

更换站点域名会改变 WebAuthn 依赖方 ID。将 `PUBLIC_ORIGIN` 改成新地址后，用恢复流程在新域名注册凭证。

## HTTP 接口

以下路由位于 `/api/admin/auth` 下。浏览器写请求必须发送与 `PUBLIC_ORIGIN` 一致的 `Origin`。完成请求发送 `{ "credential": ... }`，其中 `credential` 为 WebAuthn API 产生的凭证 JSON。启动请求返回浏览器需要的 `{ "publicKey": ... }`。认证接口响应使用 `Cache-Control: no-store`。

| 请求 | 功能 | 成功返回 |
| --- | --- | --- |
| `GET /session` | 查询管理员会话及 Passkey 可用性 | `{ "authenticated": true, "passkey_available": true }` 或相应布尔值 |
| `POST /login/start` | 为现有 Passkey 生成认证挑战 | `200`，浏览器可直接消费的 WebAuthn request options 和挑战 Cookie |
| `POST /login/finish` | 验证签名并轮换当前会话 | `200`，会话状态和管理员会话 Cookie |
| `POST /register/start` | 接收 `{ "name": "...", "enrollment_code": "..." }`，添加备份时以现有管理员会话授权并省略注册码 | `200`，浏览器可直接消费的 WebAuthn creation options 和挑战 Cookie |
| `POST /register/finish` | 验证并记录新 Passkey | `200`，会话状态和管理员会话 Cookie |
| `POST /logout` | 撤销管理员会话和该浏览器未完成的挑战 | `204`，清除管理员 Cookie |
| `GET /passkeys` | 列出有效 Passkey 的公开标识、名称与使用时间 | `200`，`[{ "id": "...", "name": "...", "created_at": 0, "last_used_at": null }]`，时间为 Unix 秒 |
| `DELETE /passkeys/{id}` | 撤销一把 Passkey 与其会话 | `204`，尝试移除最后一把时返回 `409` |

无效凭证、过期挑战和重复完成返回 `401`。HTTP Origin 不一致返回 `403`。域名或协议的开发环境配置无法使用 Passkey 时，启动和完成认证返回 `503`。

## 持久化与并发

- `admin_account` 存储管理员的随机用户标识。所有 Passkey 都绑定此身份。
- `admin_passkeys` 保存 WebAuthn 公钥凭证、最近使用时间、签名计数和撤销时间。完成登录时，事务再次确认凭证仍有效并检查最新计数，允许不提供计数器的同步 Passkey 使用零计数。
- `admin_ceremonies` 保存最多五分钟的服务端挑战状态，登录和注册使用不同的挑战 Cookie。完成请求通过带有效期过滤的 `DELETE … RETURNING` 取回且消耗挑战，签名验证失败也不能提交同一挑战。每种挑战最多保留 256 条待完成记录。
- `admin_sessions` 保存会话令牌的 SHA-256 摘要、绑定凭证、Origin 与八小时绝对有效期。生产 Cookie 使用 `Secure`、`HttpOnly`、`SameSite=Strict` 和 `__Host-` 前缀。登录或注册成功会撤销浏览器提交的旧管理员会话。
- `admin_enrollment_tickets` 保存注册码摘要。成功注册时，消耗注册码、插入新凭证、签发会话和写入审计事件在同一事务完成。成功恢复时，该事务同时撤销所有旧管理员凭证、会话和待完成挑战。

## 验收

```sh
cargo test --workspace admin_auth::tests
```

端到端测试用 WebAuthn 软件认证器签署挑战，验证首次注册、挑战并发重放、浏览器 Cookie 绑定、HTTP 与 WebAuthn Origin、会话轮换与过期、角色隔离、最后一把 Passkey 保护和成功恢复后的整体撤销。可以通过管理员界面在实际的 HTTPS 域名下执行一次设备注册、退出和登录，再添加备份设备验证目标浏览器与认证器的组合。
