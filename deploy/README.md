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

## 一键安装 agent

server 会在 `/install.sh` 公开提供安装脚本（登录要求不影响该地址）。在被监控机器上执行：

```bash
curl -fsSL https://<面板地址>/install.sh | sh -s -- --server https://<面板地址> --token <token>
```

token 用 `minimon-server node add <name>` 或后台“节点管理”获得。

- 升级：重新执行同一条命令即可。
- 卸载：`curl -fsSL https://<面板地址>/install.sh | sh -s -- --uninstall`
- 指定版本：`--version v0.1.0`
- 镜像：`--mirror <基址>` 可从镜像或代理下载发布产物（国内网络用）。注意：校验文件 `SHA256SUMS` 同样经镜像下载，因此镜像必须是可信的。

## 首次发版

```bash
git remote add origin https://github.com/neigays/minimon.git
git push -u origin main
git tag v0.1.0 && git push origin v0.1.0
```

推送 tag 后 GitHub Actions 构建并发布到 GitHub Releases。
