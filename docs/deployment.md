# 部署、升级与数据保护

本文描述单节点生产部署。当前 Gateway 使用 SQLite 与 Docker Compose named volume，**不要配置多个 Gateway 副本共用此文件**。打开多租户注册前，必须确认管理员所维护的 Context7 Key 与相应账户/Teamspace 授权允许对目标用户群提供网关服务；密钥池不可用作规避官方限额、访问限制或授权范围的回退机制。

## 生产配置

复制 `.env.example` 到 `.env`，权限设为仅部署用户可读写，并替换全部示例值。`.env` 已被 Git 忽略，不要提交或贴到聊天/工单。两项密码学秘密各自独立生成，保持服务器端加密密钥和 pepper 的离线安全备份；更换任一值都会破坏已有的上游 Key 解密或 Gateway Key 校验能力。

```sh
umask 077
cp .env.example .env
chmod 600 .env
# 每行分别用独立、密码学安全的随机 32 字节 base64url（不含 =）值替换占位符。
# OpenSSL 示例：两次调用输出不同的、独立的随机值。
openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n'
openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n'
```

必须设置：

- `APP_ENV=production`，`PUBLIC_ORIGIN=https://<DNS hostname>`：Public origin 必须是无路径、无查询的 HTTPS DNS Origin；浏览器中使用的协议、域名和端口必须匹配。Passkey Cookie 只有在 HTTPS 下才会发出。
- `GATEWAY_ENCRYPTION_KEY` 和 `GATEWAY_TOKEN_PEPPER`：独立的 32 字节 base64url 秘密。不要丢失、轮换或在多个环境中复用。
- Linux.do OAuth：`LINUXDO_OAUTH_ENABLED=true`，Client ID/Secret 和精确 callback `https://<hostname>/auth/linuxdo/callback`。
- `DEFAULT_UPSTREAM_POOL_ID`：准备开放自助注册前，设置到已确认授权且状态有效的池。
- `GATEWAY_EVENT_RETENTION_DAYS`：可选整数 `1..=365`，默认为 `30`。

不要在生产配置设置 `GATEWAY_ADMIN_TOKEN`；程序拒绝非空生产 Bearer token。管理员用 HTTPS Passkey 会话工作，首次注册和恢复使用部署主机本地 CLI 的一次性验证码（见 [`admin-auth.md`](admin-auth.md)）。

## Compose 启动

```sh
docker compose config --quiet
docker compose build --pull
docker compose up -d
docker compose ps
curl --fail --silent https://gateway.example.com/health/ready
```

镜像构建采用 `cargo build --release --locked`。服务默认以 UID/GID `10001`、read-only root filesystem、仅数据库卷可写、丢弃 Linux capabilities 与 `no-new-privileges` 运行。Host 端口默认只映射 `127.0.0.1:8787`，建议使用宿主机上的 TLS 反向代理作为唯一公开入口。反向代理需限制请求体、设置合理超时（MCP/SSE 请求可能较长）、只转发真实 Host/HTTPS scheme，并保护管理控制台。若把 Gateway 与代理放在隔离容器网络，改为不发布 Host 端口并通过共享受限 Docker network 接入，而不要在互联网接口上直接发布无 TLS 的 `8787`。

本仓库基线已在 2026-11-22 完成一次镜像构建与生产模式 Compose 冒烟：`/health/ready` 返回 ready，数据库卷由 UID 10001 写入，容器保持只读 rootfs、cap-drop ALL 和 `no-new-privileges`。这只是构建基线证据；每次发布仍应在目标主机重新构建并执行下述备份、恢复和告警检查。

要更改端口：`GATEWAY_HOST_BIND=127.0.0.1` 仅配置绑定地址；Compose 宿主机端口可通过 `.env` 中的 `GATEWAY_HOST_PORT` 修改（默认 `8787`）。将 `GATEWAY_HOST_BIND` 改为 `0.0.0.0` 会暴露明文端口到全部网络接口，除非网络 ACL 已限制访问，否则不要这样做。

## 引导首个管理员与上游池

生产部署初期应关闭开放注册。在首次引导，若还没有已建 Pool，可临时将 `DEFAULT_UPSTREAM_POOL_ID` 设为非空 `bootstrap-pending`。先启动受 TLS 保护、仅可由运维网络访问的实例，再在 Gateway 容器中签发单次初始管理员注册码：

```sh
docker compose exec context7-gateway context7-gateway admin issue-enrollment
```

在 `https://<hostname>/admin/login` 输入注册码并完成浏览器 Passkey 注册。登录后创建获得授权的 Context7 Pool、录入与验证 Key。之后把返回的 Pool ID 写到 `DEFAULT_UPSTREAM_POOL_ID` 并重启 Compose；在上线前实际测试 OAuth 回调、Gateway Key 与获授权的上下游用法。不可通过未经支持的 SQL 写入绕过管理员流程。

若管理员 Passkey 丢失，使用 [`admin-auth.md`](admin-auth.md) 中的 `--recovery` 流程重新绑定；旧 Passkey 和会话在新 Passkey 验证成功后一起撤销。

## 在线备份

使用 `scripts/backup_sqlite.sh` 的 SQLite 在线备份 API。此操作可在 Gateway 运行时执行；目标文件不能已存在，确保备份目录受限并纳入加密异机备份。

```sh
mkdir -p backups
chmod 700 backups
./scripts/backup_sqlite.sh data/context7_gateway.sqlite3 "backups/gateway-$(date -u +%Y%m%dT%H%M%SZ).sqlite3"
```

Compose named volume 不直接出现在宿主机工作目录，可先在卷内生成，再取出到加密备份位置：

```sh
name="gateway-$(date -u +%Y%m%dT%H%M%SZ).sqlite3"
docker compose exec -T context7-gateway /usr/local/bin/backup_sqlite.sh \
  /srv/gateway/data/context7_gateway.sqlite3 "/srv/gateway/data/$name"
mkdir -p backups && chmod 700 backups
docker compose cp "context7-gateway:/srv/gateway/data/$name" "backups/$name"
```

定时任务示例（以运维调度器配置运行用户与加密备份传输）：每天 `02:15 UTC` 生成唯一文件名。备份中包含 OAuth 用户映射、Passkey 公钥、会话/审计/计量信息与加密后的上游 Key；所以备份属于敏感生产数据，限定最小访问权限。**只有数据库而没有完全匹配的 `GATEWAY_ENCRYPTION_KEY` 和 `GATEWAY_TOKEN_PEPPER`，恢复不是有效服务恢复**；两项秘密要单独备份并演练。

## 恢复演练

在临时主机或本地测试环境中验证，切勿覆盖生产目标。恢复脚本要求停止所有连到目标 DB 的 Gateway 进程，先检查源备份，再安装，并通过 SQLite Online Backup API 将当前数据库（包含已提交 WAL 内容）另存为带时间的安全副本。

```sh
name="gateway-<timestamp>.sqlite3"
# 先复制到服务卷，再停掉所有使用目标 DB 的进程。
docker compose cp "backups/$name" "context7-gateway:/srv/gateway/data/restore.sqlite3"
docker compose stop context7-gateway
docker compose run --rm --no-deps --entrypoint /usr/local/bin/restore_sqlite.sh \
  context7-gateway /srv/gateway/data/context7_gateway.sqlite3 /srv/gateway/data/restore.sqlite3
docker compose run --rm --no-deps --entrypoint sqlite3 context7-gateway \
  /srv/gateway/data/context7_gateway.sqlite3 'PRAGMA integrity_check;'
docker compose up -d context7-gateway
curl --fail --silent https://gateway.example.com/health/ready
docker compose logs --since=5m context7-gateway
```

验收标准是 `integrity_check` 输出 `ok`，服务健康，用户可正常登录，已存上游 Key 可解密/验证，用户 Gateway Key 可通过鉴权。恢复结果若与备份中持有的密码学秘密不匹配，应立即停服并恢复正确秘密，禁止重置密钥来“修复”数据。

## 日志与事件保留

`GATEWAY_EVENT_RETENTION_DAYS` 控制 `usage_events` 保留时间。启动后每 60 秒，后台任务最多回收 256 条过期预留并删除 4096 条过期事件，避免单次锁表过久；保留窗口使用 UTC 秒数并以事件记录时间为准。配额聚合 `quota_counters` 和管理员安全审计事件不在本设置内删除；备份/存储容量应按其增长另行监控。该清理不替代备份保留策略、SQLite VACUUM 评估或对用户告知日志保留期限。

## 发布与回滚

1. 发布前审阅本次 SQLx Migration、依赖审计、CI 测试和 SBOM；对新依赖和权限边界进行人工检查。
2. 为生产 DB 创建并校验在线备份；另存运行镜像摘要、`.env` 加密副本及当前迁移版本。
3. 使用不可变镜像标签/摘要部署，观察 `/health/ready`、5xx/429、DB 可写性、反向代理 TLS 和管理员登录。
4. **二进制回滚不会反向迁移数据库**。若新版本执行了前向迁移，只能在兼容性确认后按旧版可读格式上线，或在停服状态恢复迁移前数据库备份，并同步匹配的旧版本密码学秘密；不得简单把旧镜像指向已改写 DB。
5. 记录发布人、摘要、迁移版本、备份位置、健康检查及回滚决定。

## CI 安全产物

`.github/workflows/security.yml` 在 PR 检查依赖审计，并生成 CycloneDX SBOM 作为构建 artifact。扫描结果和 SBOM 不能代替升级审批、供应链来源验证或运行镜像扫描；有阻断告警时需先升级或记录针对该版本的书面例外。
