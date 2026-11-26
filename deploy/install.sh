#!/bin/sh
# minimon agent 一键安装脚本（Linux systemd / macOS launchd）。
#
#   curl -fsSL https://<面板地址>/install.sh | sudo bash -s -- --server wss://<面板地址> --token <token>
#
# 重复执行 = 升级二进制 + 覆盖配置 + 重启服务。卸载：同一命令改为 --uninstall。
#
# 仅测试用的环境变量（不对用户宣传）：
#   MINIMON_INSTALL_ROOT     所有安装路径加此前缀
#   MINIMON_SKIP_SERVICE=1   不调用 systemctl / launchctl
#   MINIMON_RELEASE_BASE     替代 GitHub Releases 下载基址
#   MINIMON_SKIP_ROOT_CHECK=1  跳过 root 检查
#   MINIMON_UNAME_S / MINIMON_UNAME_M  覆盖 uname -s / uname -m
#
# 结构约定：文件只包含常量与函数定义，唯一的顶层执行语句是最后一行 `main "$@"`。
# 这样 `curl | bash` 中途断开、只收到半个脚本时什么都不会执行。
set -eu

RELEASE_BASE=${MINIMON_RELEASE_BASE:-https://github.com/neigays/minimon/releases}
ROOT=${MINIMON_INSTALL_ROOT:-}
SKIP_SERVICE=${MINIMON_SKIP_SERVICE:-0}

BIN_PATH=/usr/local/bin/minimon-agent
ENV_DIR=/etc/minimon
ENV_FILE=$ENV_DIR/agent.env
UNIT_PATH=/etc/systemd/system/minimon-agent.service
PLIST_PATH=/Library/LaunchDaemons/com.minimon.agent.plist
LABEL=com.minimon.agent
MAC_LOG=/var/log/minimon-agent.log

SERVER=
TOKEN=
MIRROR=
VERSION=
UNINSTALL=0

# 退出时清理：下载临时目录与尚未 mv 到位的暂存文件。
DL_DIR=
STAGED=
cleanup() {
    if [ -n "$STAGED" ]; then rm -f "$STAGED"; fi
    if [ -n "$DL_DIR" ]; then rm -rf "$DL_DIR"; fi
}

info() { printf '%s\n' "$*"; }
die() {
    printf '错误：%s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
minimon agent 安装脚本

用法：
  curl -fsSL <面板地址>/install.sh | sudo bash -s -- --server wss://<面板地址> --token <token>

参数：
  --server <ws(s)://…>   面板地址（安装时必填）
  --token <token>        节点 token（安装时必填）
  --mirror <前缀>        下载地址前缀（GitHub 加速镜像），如 https://ghproxy.example/
  --version <x.y.z>      安装指定版本，默认最新版
  --uninstall            停止服务并删除二进制、服务文件与配置
  --help                 显示本帮助

重复执行即升级：覆盖二进制与配置并重启服务。
EOF
}

# ---------------- 参数 ----------------

need_value() {
    # $1 = 参数名，$2 = 剩余参数个数
    [ "$2" -ge 2 ] || die "参数 $1 缺少值"
}

parse_args() {
    while [ $# -gt 0 ]; do
        case $1 in
            --server) need_value "$1" $#; SERVER=$2; shift 2 ;;
            --server=*) SERVER=${1#*=}; shift ;;
            --token) need_value "$1" $#; TOKEN=$2; shift 2 ;;
            --token=*) TOKEN=${1#*=}; shift ;;
            --mirror) need_value "$1" $#; MIRROR=$2; shift 2 ;;
            --mirror=*) MIRROR=${1#*=}; shift ;;
            --version) need_value "$1" $#; VERSION=$2; shift 2 ;;
            --version=*) VERSION=${1#*=}; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            -h | --help) usage; exit 0 ;;
            # 未知参数可能就是拼错的 --token=<值> 或误放的 token 本身：只回显参数名，不回显值。
            --*=*) die "未知参数：${1%%=*}（用 --help 查看用法）" ;;
            -*) die "未知参数：${1}（用 --help 查看用法）" ;;
            *) die "未知参数（用 --help 查看用法）" ;;
        esac
    done
}

# 写入 agent.env 的值会被 systemd EnvironmentFile 与 launchd 包装命令里的 sh 解析，
# 因此只允许安全字符：不含空白、引号、$、`、\、;、& 等。
validate_install_args() {
    [ -n "$SERVER" ] || die "缺少 --server（面板地址，如 wss://example.com）"
    [ -n "$TOKEN" ] || die "缺少 --token（在面板后台 → 节点 中获取）"
    case $SERVER in
        ws://?* | wss://?*) ;;
        *) die "--server 必须以 ws:// 或 wss:// 开头" ;;
    esac
    case $SERVER in
        *[!A-Za-z0-9._:/@%+=?#,\[\]-]*) die "--server 含有不允许的字符" ;;
    esac
    case $TOKEN in
        *[!A-Za-z0-9_-]*) die "--token 含有不允许的字符（只允许字母、数字、_、-）" ;;
    esac
    case $MIRROR in
        *[!A-Za-z0-9._:/@%+=?#,~-]*) die "--mirror 含有不允许的字符" ;;
    esac
    if [ -n "$VERSION" ]; then
        VERSION=${VERSION#v}
        case $VERSION in
            [0-9]*) ;;
            *) die "--version 格式应为 x.y.z 或 vx.y.z" ;;
        esac
        case $VERSION in
            *[!0-9A-Za-z.-]*) die "--version 格式应为 x.y.z 或 vx.y.z" ;;
        esac
    fi
}

# ---------------- root 与平台 ----------------

check_root() {
    if [ "${MINIMON_SKIP_ROOT_CHECK:-0}" != 1 ] && [ "$(id -u)" -ne 0 ]; then
        die "需要 root 权限，请用 sudo 重新执行（例如 curl … | sudo bash -s -- …）"
    fi
}

detect_platform() {
    UNAME_S=${MINIMON_UNAME_S:-$(uname -s)}
    UNAME_M=${MINIMON_UNAME_M:-$(uname -m)}
    case $UNAME_S in
        Linux) OS=linux ;;
        Darwin) OS=darwin ;;
        *) die "不支持的平台：${UNAME_S}/${UNAME_M}" ;;
    esac
    case $UNAME_M in
        x86_64 | amd64) ARCH=x86_64 ;;
        aarch64 | arm64) ARCH=aarch64 ;;
        *) ARCH=unknown ;;
    esac
}

# ---------------- 服务管理 ----------------

has_systemd() {
    command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]
}

service_enabled() {
    [ "$SKIP_SERVICE" != 1 ]
}

# 原子写入：先写同目录下的暂存文件，再 mv 覆盖。内容来自 stdin。
write_atomic() {
    # $1 = 目标路径，$2 = 权限
    dst=$1
    STAGED=$(dirname "$dst")/.$(basename "$dst").new
    rm -f "$STAGED"
    cat >"$STAGED"
    chmod "$2" "$STAGED"
    mv -f "$STAGED" "$dst"
    STAGED=
}

write_unit() {
    write_atomic "$1" 644 <<'EOF'
[Unit]
Description=minimon agent
After=network-online.target
Wants=network-online.target

[Service]
# 内容：MINIMON_SERVER=wss://...  MINIMON_TOKEN=...（权限 600）
EnvironmentFile=/etc/minimon/agent.env
ExecStart=/usr/local/bin/minimon-agent
Restart=always
RestartSec=5
TimeoutStopSec=10
DynamicUser=yes
NoNewPrivileges=yes
# 免特权 ICMP（ping_group_range）不可用时退回 RAW 套接字，需要 CAP_NET_RAW；
# 仅授予 ambient 能力，并用 CapabilityBoundingSet 把能力上限收到 CAP_NET_RAW。
AmbientCapabilities=CAP_NET_RAW
CapabilityBoundingSet=CAP_NET_RAW
ProtectSystem=strict
ProtectHome=yes

[Install]
WantedBy=multi-user.target
EOF
}

write_plist() {
    write_atomic "$1" 644 <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.minimon.agent</string>
    <!-- launchd 不支持 EnvironmentFile：由 sh 读取 /etc/minimon/agent.env（权限 600），token 不写进 plist -->
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>set -a; . /etc/minimon/agent.env; exec /usr/local/bin/minimon-agent</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/minimon-agent.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/minimon-agent.log</string>
</dict>
</plist>
EOF
}

# ---------------- 卸载 ----------------

uninstall() {
    if [ "$OS" = linux ]; then
        if service_enabled && command -v systemctl >/dev/null 2>&1; then
            systemctl disable --now minimon-agent >/dev/null 2>&1 || true
        fi
        rm -f "$ROOT$UNIT_PATH"
        if service_enabled && has_systemd; then
            systemctl daemon-reload || true
        fi
    else
        if service_enabled; then
            launchctl bootout "system/$LABEL" >/dev/null 2>&1 || true
        fi
        rm -f "$ROOT$PLIST_PATH" "$ROOT$MAC_LOG"
    fi
    rm -f "$ROOT$BIN_PATH" "$ROOT$ENV_FILE"
    # 目录里还有别的文件（如 server.env）时保留
    rmdir "$ROOT$ENV_DIR" 2>/dev/null || true
    info "minimon-agent 已卸载"
}


# ---------------- 下载与校验 ----------------

fetch() {
    # $1 = URL，$2 = 输出文件
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --connect-timeout 15 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -T 15 -t 3 -O "$2" "$1"
    else
        die "需要 curl 或 wget"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "需要 sha256sum 或 shasum 来校验下载文件"
    fi
}

# 下载 $FILE 与 SHA256SUMS 到 $DL_DIR 并校验；失败时已安装文件不受影响。
download_and_verify() {
    FILE=minimon-agent-$OS-$ARCH
    case $MIRROR in
        '' | */) ;;
        *) MIRROR=$MIRROR/ ;;
    esac
    if [ -n "$VERSION" ]; then
        URL_DIR=$MIRROR$RELEASE_BASE/download/v$VERSION
    else
        URL_DIR=$MIRROR$RELEASE_BASE/latest/download
    fi

    DL_DIR=$(mktemp -d)
    info "下载 $FILE …"
    fetch "$URL_DIR/$FILE" "$DL_DIR/$FILE" || die "下载失败：$URL_DIR/$FILE"
    fetch "$URL_DIR/SHA256SUMS" "$DL_DIR/SHA256SUMS" || die "下载失败：$URL_DIR/SHA256SUMS"

    # SHA256SUMS 为 sha256sum 输出格式：<hash>  <文件名>（二进制模式下文件名前带 *）
    EXPECTED=$(awk -v f="$FILE" '$2 == f || $2 == "*" f { print tolower($1); exit }' "$DL_DIR/SHA256SUMS")
    [ -n "$EXPECTED" ] || die "校验失败：SHA256SUMS 中没有 $FILE"
    ACTUAL=$(sha256_of "$DL_DIR/$FILE")
    [ "$EXPECTED" = "$ACTUAL" ] || die "校验失败：$FILE 的 SHA256 不匹配（文件损坏或镜像不可用），已中止，原有安装未改动"
}

# ---------------- 安装 ----------------

install_files() {
    mkdir -p "$ROOT$(dirname "$BIN_PATH")"
    write_atomic "$ROOT$BIN_PATH" 755 <"$DL_DIR/$FILE"

    mkdir -p "$ROOT$ENV_DIR"
    chmod 755 "$ROOT$ENV_DIR"
    # 用 printf 内建写入，token 不会出现在进程参数里；umask 077 保证文件从创建起就不可被他人读取。
    STAGED=$ROOT$ENV_DIR/.agent.env.new
    rm -f "$STAGED"
    (
        umask 077
        printf 'MINIMON_SERVER=%s\nMINIMON_TOKEN=%s\n' "$SERVER" "$TOKEN" >"$STAGED"
    )
    chmod 600 "$STAGED"
    mv -f "$STAGED" "$ROOT$ENV_FILE"
    STAGED=
}

register_service() {
    if [ "$OS" = linux ]; then
        mkdir -p "$ROOT$(dirname "$UNIT_PATH")"
        write_unit "$ROOT$UNIT_PATH"
        if service_enabled; then
            systemctl daemon-reload || die "systemctl daemon-reload 失败"
            systemctl enable minimon-agent >/dev/null 2>&1 || die "systemctl enable minimon-agent 失败"
            systemctl restart minimon-agent || die "启动失败，查看日志：journalctl -u minimon-agent -e"
        fi
        STATUS_CMD="systemctl status minimon-agent"
        LOG_CMD="journalctl -u minimon-agent -f"
    else
        mkdir -p "$ROOT$(dirname "$PLIST_PATH")"
        write_plist "$ROOT$PLIST_PATH"
        if service_enabled; then
            launchctl bootout "system/$LABEL" >/dev/null 2>&1 || true
            launchctl enable "system/$LABEL" >/dev/null 2>&1 || true
            # bootout 后服务可能尚未完全卸载，bootstrap 偶发失败，重试几次
            boot_try=1
            until launchctl bootstrap system "$ROOT$PLIST_PATH"; do
                [ "$boot_try" -lt 3 ] || die "launchctl bootstrap 失败"
                boot_try=$((boot_try + 1))
                sleep 1
            done
        fi
        STATUS_CMD="sudo launchctl print system/$LABEL"
        LOG_CMD="tail -f $MAC_LOG"
    fi
}

# ---------------- 入口 ----------------

main() {
    # 新建的二进制、unit、目录一律 755/644；agent.env 单独用 077 创建。
    umask 022
    trap cleanup EXIT
    trap 'exit 1' HUP INT TERM

    parse_args "$@"
    if [ "$UNINSTALL" -eq 0 ]; then
        validate_install_args
    fi
    check_root
    detect_platform

    if [ "$UNINSTALL" -eq 1 ]; then
        uninstall
        exit 0
    fi

    if [ "$ARCH" = unknown ] || [ "$OS-$ARCH" = darwin-x86_64 ]; then
        die "不支持的平台：${UNAME_S}/${UNAME_M}（仅支持 linux-x86_64、linux-aarch64、darwin-aarch64）"
    fi
    # 在改动任何文件之前确认服务管理器可用，避免装了一半。
    if [ "$OS" = linux ] && service_enabled && ! has_systemd; then
        die "未检测到 systemd。请从 GitHub Releases 手动下载 minimon-agent-${OS}-${ARCH}，用 MINIMON_SERVER=… MINIMON_TOKEN=… minimon-agent 运行"
    fi

    download_and_verify
    install_files
    register_service

    INSTALLED_VERSION=$("$ROOT$BIN_PATH" --version 2>/dev/null | head -n 1) || true
    [ -n "$INSTALLED_VERSION" ] || INSTALLED_VERSION=${VERSION:-最新版}

    info ""
    info "minimon-agent 安装完成：$INSTALLED_VERSION"
    info "  面板地址：$SERVER"
    info "  查看状态：$STATUS_CMD"
    info "  查看日志：$LOG_CMD"
    info "  卸载：重新执行本脚本并加 --uninstall"
}

# 必须是文件最后一行：脚本未完整下载时不会执行任何操作。
main "$@"
