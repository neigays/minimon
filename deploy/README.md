# 部署

## Docker 部署 server

在仓库根目录执行：

```bash
docker compose -f deploy/docker-compose.yml up -d --build
docker logs minimon   # 首次启动会打印一次管理员初始密码
```

数据在命名卷 `minimon-data`（容器内 `/data`）。

### 升级

升级前先备份数据卷：

```bash
docker run --rm -v minimon-data:/data -v "$PWD":/backup alpine \
  tar czf /backup/minimon-data-$(date +%F).tgz -C /data .
```

然后拉取新代码，重新构建并启动：

```bash
git pull
docker compose -f deploy/docker-compose.yml up -d --build
```

## systemd 部署 server

1. 把 `minimon-server` 二进制放到 `/usr/local/bin/minimon-server`。
2. （可选）创建 `/etc/minimon/server.env`，写入 `MINIMON_LISTEN`、`MINIMON_PUBLIC_URL`、`MINIMON_ADMIN_PASSWORD` 等环境变量。
3. 安装并启动：

```bash
sudo cp deploy/minimon-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now minimon-server
```

数据目录为 `/var/lib/minimon`（由 systemd 的 `StateDirectory` 管理）。首次初始密码见 `journalctl -u minimon-server`。

systemd 部署下不要在 shell 里直接运行 `minimon-server node add` 或 `admin reset-password`：它们会按当前目录解析 `./data`，而不是 `/var/lib/minimon`；用 root 加 `MINIMON_DATA_DIR` 运行还会在 DynamicUser 的状态目录里留下 root 所有的 `-wal`/`-shm` 文件。

- 新增节点：在管理后台 → 节点 → 新增。
- 重置管理员密码（先停服务，再用与服务相同的动态用户运行，新密码打印在终端）：

```bash
sudo systemctl stop minimon-server && \
sudo systemd-run --pty --wait -p DynamicUser=yes -p StateDirectory=minimon \
  -E MINIMON_DATA_DIR=/var/lib/minimon /usr/local/bin/minimon-server admin reset-password && \
sudo systemctl start minimon-server
```

## 一键安装 agent

server 会在 `/install.sh` 公开提供安装脚本（登录要求不影响该地址）。在被监控机器上执行：

```bash
curl -fsSL https://<面板地址>/install.sh | sudo bash -s -- --server wss://<面板地址> --token <token>
```

安装脚本需要 root（所以用 `sudo`），`--server` 只接受 `ws://` 或 `wss://`：面板是明文 HTTP 时用 `ws://<面板地址>`，HTTPS 时用 `wss://`。

token 在后台“节点管理”新增节点获得（Docker 部署也可用 `docker exec minimon minimon-server node add <name>`）。

注意：`sudo` 会把完整命令（包括 `--token`）记录到系统认证日志。如果 token 可能已泄露，在后台 节点 → 重置 token，然后重新执行安装命令。

`--server` 也接受 IPv6 字面量地址，如 `ws://[2001:db8::1]:25780`。

- 升级：重新执行同一条命令即可。
- 卸载：`curl -fsSL https://<面板地址>/install.sh | sudo bash -s -- --uninstall`
- 指定版本：`--version v0.1.0`
- 镜像：`--mirror <前缀>`（如 `https://ghproxy.example/`）会加在下载地址前，从镜像或代理下载发布产物（国内网络用）。注意：校验文件 `SHA256SUMS` 同样经镜像下载，因此镜像必须是可信的。

## 首次发版

```bash
git remote add origin https://github.com/neigays/minimon.git
git push -u origin main
git tag v0.1.0 && git push origin v0.1.0
```

之后发新版本：改根 `Cargo.toml` 的 workspace version → 运行 `cargo check`（更新 `Cargo.lock`）→ 一起提交 `Cargo.toml` 和 `Cargo.lock` → 再打 tag。否则 `cargo zigbuild --locked` 会失败。

推送 tag 后 GitHub Actions 构建并发布到 GitHub Releases。
