# Linux.do Connect OAuth 配置

Gateway 使用 Linux.do Connect 作为普通用户登录身份源。Linux.do 只控制用户控制台和 Gateway Key 自助管理，不提供管理员权限。管理员登录是独立的环境变量用户名/密码认证，Passkey 为登录后可选绑定；两者都不经过 Linux.do。

## 1. 创建 Linux.do OAuth 应用

1. 打开 [Linux.do Connect](https://connect.linux.do/)。
2. 进入“我的应用接入”或“申请新接入”，创建 OAuth 应用。
3. 应用名称可填写 `Context7 Gateway`。
4. 填写与部署地址完全一致的回调地址：

   ```text
   http://localhost:8787/auth/linuxdo/callback
   ```

   生产环境示例：

   ```text
   https://gateway.example.com/auth/linuxdo/callback
   ```

5. Scope 选择 `openid profile`。本项目默认不需要邮箱。
6. 保存后记录 `Client ID` 和 `Client Secret`。Client Secret 只放在服务端环境变量中，不提交到 Git、不写入前端。

## 2. 配置环境变量

```sh
LINUXDO_OAUTH_ENABLED=true
LINUXDO_OAUTH_CLIENT_ID=你的_client_id
LINUXDO_OAUTH_CLIENT_SECRET=你的_client_secret
LINUXDO_OAUTH_REDIRECT_URL=http://localhost:8787/auth/linuxdo/callback
LINUXDO_OAUTH_SCOPE="openid profile"
```

以下端点有官方默认值，通常不需要填写；只有切换测试环境时才覆盖：

```sh
# LINUXDO_OAUTH_AUTHORIZE_URL=https://connect.linux.do/oauth2/authorize
# LINUXDO_OAUTH_TOKEN_URL=https://connect.linux.do/oauth2/token
# LINUXDO_OAUTH_USERINFO_URL=https://connect.linux.do/api/user
```

同时必须满足：

- `PUBLIC_ORIGIN` 与回调地址的协议、主机和端口完全一致。
- 若还要在管理端绑定或使用 Passkey，本地预览使用 `PUBLIC_ORIGIN=http://localhost:8787`，不要混用 `127.0.0.1`；Passkey 是可选能力。
- `GATEWAY_TOKEN_PEPPER` 已配置。OAuth state、PKCE 和用户会话都依赖它；缺失时登录接口返回 `503 not_configured`。
- 生产环境必须使用 HTTPS DNS 域名，`LINUXDO_OAUTH_REDIRECT_URL` 必须等于 `PUBLIC_ORIGIN + /auth/linuxdo/callback`，且不能带 query。

修改环境变量后重启 Gateway。

## 3. 配置默认注册池

OAuth 配置好之后，新用户还不会自动进入，因为注册默认关闭。

```sh
DEFAULT_UPSTREAM_POOL_ID=你的_pool_id
```

开启注册前必须：

1. 该 Pool 已启用。
2. Pool 中至少有一把验证成功、状态为 `active` 的上游 Key。
3. Pool 没有处于限流冷却或熔断状态。

批量导入的上游 Key 初始为 `pending`，需要先在管理端逐把执行 **Verify key**。否则管理员设置页无法打开注册。

## 4. 打开用户注册

用管理员账号登录。默认使用 `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD` 密码登录；如果此前绑定过 Passkey，登录页也会显示可选的 **Sign in with passkey**：

```text
http://localhost:8787/admin/login
```

进入 **Settings**，打开 **User registration**。后端会再次检查 OAuth、默认 Pool 和加密配置；检查失败时不会打开注册，并返回 `not_configured`。

关闭注册只阻止新用户自动创建；已经绑定过 Linux.do 身份的现有用户可以继续登录。新用户首次通过 OAuth 注册时，Gateway 自动签发一把默认包含 MCP 与 REST scope 的 API Key。用户控制台始终只显示掩码，点击复制按钮时才由后端解密并写入剪贴板；用户端不能自行创建、轮换或撤销 API Key。管理员也可以保持注册关闭，只在 **Users** 中为已批准用户签发 Gateway Key。

## 5. 验证登录

访问：

```text
http://localhost:8787/auth/linuxdo
```

或从用户控制台点击 **Sign in with Linux.do**。完整流程应为：

```text
Gateway -> connect.linux.do 授权页 -> /auth/linuxdo/callback -> /console/dashboard
```

回调成功后，Gateway 会：

- 校验 state Cookie 和 PKCE S256 verifier。
- 使用授权码换取 access token。
- 读取 `/api/user`，拒绝 `active=false` 或 `silenced=true` 的账户。
- 按 Linux.do 用户 ID 和 userinfo origin 绑定本地用户。
- 保存合法的 `trust_level`（0–4）；新用户按下表自动获得初始配额。
- 创建短期登录会话并设置 HttpOnly Cookie。

## 6. Linux.do 等级配额

新注册用户会根据 userinfo 中的 `trust_level` 创建第一版 `user_quota_assignments`：

| 等级 | 每 5 分钟请求 | 每小时业务调用 | 每日积分 | 每月积分 |
| --- | ---: | ---: | ---: | ---: |
| L0 | 10 | 30 | 10 | 50 |
| L1 | 30 | 60 | 20 | 300 |
| L2 | 60 | 240 | 70 | 1,000 |
| L3 | 120 | 600 | 200 | 3,000 |
| L4 | 180 | 900 | 400 | 6,000 |

后续登录发现等级变化时，只在当前计划仍等于旧等级默认值（且未配置操作权重）时自动切换；管理员手工改过的配额保持不变。`trust_level` 缺失或不在 `0..=4` 时不会创建默认计划，也不会清除已有等级或配额。

## 7. 常见错误

| 现象 | 原因 | 处理 |
|---|---|---|
| 点击登录返回 `503 not_configured` | OAuth 未启用、缺少 Client ID/Secret、缺少 `GATEWAY_TOKEN_PEPPER` | 检查环境变量并重启 |
| Linux.do 显示回调地址不匹配 | 控制台回调与 `LINUXDO_OAUTH_REDIRECT_URL` 不一致 | 两处都改成完全相同的 URL，含端口和路径 |
| 回调返回 `401 unauthorized` | state Cookie 丢失/过期、回调用了不同主机名、重复回调 | 使用同一 Origin，关闭阻止 Cookie 的插件后重试 |
| 新用户回调返回 `403 registration_closed` | 注册开关关闭，或默认 Pool 无 eligible Key | 先验证上游 Key，再打开注册 |
| 从 `localhost` 换成 `127.0.0.1` 后用户登录失败 | OAuth state/Cookie 使用了不同 Origin | 保持使用同一个 Origin；管理员 Passkey（如已绑定）也受 WebAuthn Origin 约束 |
| 修改域名后老用户无法匹配 | 用户绑定使用 issuer + subject | 保留原 userinfo origin，或执行明确的数据迁移 |

## 8. 生产检查清单

- Linux.do 应用回调地址与 `PUBLIC_ORIGIN` 完全一致。
- TLS 终止在可信反向代理，转发真实 Host 和 HTTPS scheme。
- Client Secret 只存在于部署平台的 Secret/环境变量中。
- 管理员默认使用 `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD`；两项同时设置或同时留空，`.env` 权限为 `600`。Passkey 可在登录后从 Settings 可选绑定。
- `GATEWAY_ENCRYPTION_KEY` 和 `GATEWAY_TOKEN_PEPPER` 已独立备份且保持稳定。
- 默认 Pool 有已激活的上游 Key，且池未处于限流/熔断状态。
- 开放注册前先完成一次真实 Linux.do 用户登录和控制台跳转测试，并单独验证管理员密码/可选 Passkey 登录。
