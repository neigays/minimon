# minimon

轻量级自托管服务器监控。一个 server 二进制（内嵌前端和 SQLite），一个 agent 二进制，不依赖任何外部服务。

适合个人运维者监控几台到几十台 VPS、代理节点或家用服务器，包括在 NAT 后面的机器。

## 功能

- **实时卡片墙**：CPU、内存、Swap、磁盘、网速、负载、在线时长，秒级刷新
- **历史曲线**：1 小时 / 24 小时 / 7 天 / 30 天，自动降采样
- **延迟探测**：ICMP / TCP / HTTP，后台一键创建三网（电信 / 联通 / 移动）探测
- **月流量统计**：额度、重置日、计费口径（入 / 出 / 入+出 / 取较大者）
- **到期提醒**：到期前 7 / 3 / 1 天提醒
- **告警**：离线、资源阈值、探测延迟 / 丢包、流量 80% / 100%，通过 [Server酱³](https://sc3.ft07.com/) 推送；带恢复通知、冷却和合并发送
- **访问控制**：公开只读页 + 单管理员后台；IP、主机名、探测目标、流量、到期日等敏感字段只对管理员可见，在服务端过滤

## 设计取舍

- agent 主动用 WebSocket 连 server，**不监听任何端口**，NAT 后的机器也能接入
- agent **不执行任何远程命令**，只处理协议里固定的几种消息
- 存储用 SQLite（WAL），按 raw 24 小时 / 1 分钟聚合 7 天 / 15 分钟聚合 90 天分级保留
- 不做主题、插件、Web SSH、agent 自动更新

| 项目 | 体积 / 资源上限 |
|------|------|
| agent 二进制 | 5 MB |
| agent 内存 | 15 MB |
| server 二进制（含前端） | 20 MB |
| server 内存（50 节点） | 80 MB |

## 快速开始

### 1. 部署 server（Docker）

```bash
git clone https://github.com/neigays/minimon.git
cd minimon
docker compose -f deploy/docker-compose.yml up -d --build
```

默认监听 `25780` 端口，数据保存在 `minimon-data` 卷中。首次启动时会随机生成管理员密码并**只在日志里打印一次**：

```bash
docker logs minimon 2>&1 | grep 初始密码
```

也可以在 `deploy/docker-compose.yml` 中用 `MINIMON_ADMIN_PASSWORD` 预设。忘记密码时：

```bash
docker exec minimon minimon-server admin reset-password
```

建议放在 HTTPS 反向代理后面，并设置 `MINIMON_PUBLIC_URL` 为对外地址（用于生成 agent 安装命令）。

### 2. 接入 agent

登录后台 → 节点 → 新增节点，复制显示的安装命令，在要监控的机器上执行：

```bash
curl -fsSL https://<面板地址>/install.sh | sudo bash -s -- --server wss://<面板地址> --token <token>
```

脚本会从 GitHub Releases 下载对应平台的 agent、校验 SHA256、注册为 systemd（Linux）或 launchd（macOS）服务。

- 国内机器下载慢：在命令末尾加 `--mirror <代理前缀>`
- 升级：重新执行同一条命令
- 卸载：`curl -fsSL https://<面板地址>/install.sh | sudo bash -s -- --uninstall`

支持的平台：Linux x86_64 / aarch64（musl 静态链接）、macOS aarch64。

ICMP 探测需要非特权 ping（`net.ipv4.ping_group_range` 覆盖 agent 的 gid）或 `CAP_NET_RAW`（systemd unit 已授予）；都没有时显示「ICMP 不可用」，TCP / HTTP 探测不受影响。

### 3. 配置告警

后台 → 告警 → 通知，填入 Server酱³ 的 SendKey，点「发送测试」确认能收到。离线、流量、到期告警默认开启，自定义阈值规则在同一页添加。

更多部署方式（systemd 部署 server、升级、发版）见 [deploy/README.md](deploy/README.md)。

## 配置

server 的启动级配置通过环境变量或 TOML 配置文件（`minimon-server serve --config <file>`）设置，其余配置都在后台修改。

| 环境变量 | 说明 | 默认值 |
|------|------|------|
| `MINIMON_LISTEN` | 监听地址 | `0.0.0.0:25780` |
| `MINIMON_DATA_DIR` | 数据目录 | `./data`（Docker 为 `/data`，systemd 为 `/var/lib/minimon`） |
| `MINIMON_PUBLIC_URL` | 对外访问地址，用于生成安装命令 | 无（按当前页面地址推断） |
| `MINIMON_REAL_IP_HEADER` | 取真实 IP 的请求头，仅在可信反向代理后设置 | 无 |
| `MINIMON_ADMIN_PASSWORD` | 首次启动时的管理员密码 | 随机生成 |

## 从源码构建

需要 Rust 1.97+ 和 Node.js 22.18+。

```bash
cd web && npm ci && npm run build && cd ..   # 先构建前端，server 编译时会嵌入
cargo build --release
```

产物在 `target/release/minimon-server` 和 `target/release/minimon-agent`。

开发时：

```bash
cargo run -p minimon-server -- serve --config dev/server.toml
cd web && npm run dev                        # Vite 代理 /api 到本地 server
cargo test --workspace && (cd web && npm test)
```

## 技术栈

Rust（axum、tokio、rusqlite、rust-embed）· Vue 3 + Vite + uPlot
