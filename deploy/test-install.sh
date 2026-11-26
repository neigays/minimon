#!/bin/sh
# deploy/install.sh 的测试：本地起一个假 release 服务器（python3），
# 把安装根目录指向临时目录，逐个用例校验落盘结果与错误处理。
# 用法：sh deploy/test-install.sh（macOS 与 Linux 均可运行，不需要 root）
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
INSTALL_SH="$HERE/install.sh"
WORK=$(mktemp -d)
SRV_PID=

cleanup() {
    if [ -n "$SRV_PID" ]; then
        kill "$SRV_PID" 2>/dev/null || true
        wait "$SRV_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

WWW="$WORK/www"         # 假服务器提供的文件
BIN="$WORK/bin"         # 测试自己的脚本
ACCESS_LOG="$WORK/access.log"
mkdir -p "$WWW" "$BIN"
: >"$ACCESS_LOG"

# 假 release 服务器：记录每个请求路径；按路径最后一段文件名返回 www 下的文件，
# 因此 /releases/latest/download/x、/releases/download/v0.2.0/x、
# /m/http://…/x 都能拿到同一个文件，路径形式由访问日志断言。
cat >"$BIN/fake_release.py" <<'EOF'
import http.server
import os
import sys

www, log, port_file = sys.argv[1], sys.argv[2], sys.argv[3]


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(log, "a") as f:
            f.write(self.path + "\n")
        name = self.path.rsplit("/", 1)[-1]
        path = os.path.join(www, name)
        if not name or name.startswith(".") or not os.path.isfile(path):
            self.send_error(404)
            return
        with open(path, "rb") as f:
            data = f.read()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


srv = http.server.HTTPServer(("127.0.0.1", 0), Handler)
with open(port_file + ".tmp", "w") as f:
    f.write(str(srv.server_address[1]))
os.rename(port_file + ".tmp", port_file)
srv.serve_forever()
EOF

python3 -I "$BIN/fake_release.py" "$WWW" "$ACCESS_LOG" "$WORK/port" &
SRV_PID=$!
i=0
while [ ! -s "$WORK/port" ]; do
    i=$((i + 1))
    if [ "$i" -gt 100 ]; then
        echo "FAIL: 假 release 服务器未能启动" >&2
        exit 1
    fi
    sleep 0.1
done
PORT=$(cat "$WORK/port")
BASE_URL="http://127.0.0.1:$PORT"

sha256_line() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1"
    else
        shasum -a 256 "$1"
    fi
}

# 发布假产物：内容是打印 "fake <ver>" 的 sh 脚本，并生成 SHA256SUMS。
publish() {
    for f in minimon-agent-linux-x86_64 minimon-agent-linux-aarch64 minimon-agent-darwin-aarch64; do
        printf '#!/bin/sh\necho "fake %s"\n' "$1" >"$WWW/$f"
    done
    (cd "$WWW" && for f in minimon-agent-*; do sha256_line "$f"; done) >"$WWW/SHA256SUMS"
}

# 每个用例的状态
ROOT=
OUT=
UNAME_S=Linux
UNAME_M=x86_64

# 以测试开关运行 install.sh；输出写入 $OUT，返回 install.sh 的退出码。
run_install() {
    env MINIMON_INSTALL_ROOT="$ROOT" \
        MINIMON_SKIP_SERVICE=1 \
        MINIMON_SKIP_ROOT_CHECK=1 \
        MINIMON_RELEASE_BASE="$BASE_URL/releases" \
        MINIMON_UNAME_S="$UNAME_S" \
        MINIMON_UNAME_M="$UNAME_M" \
        sh "$INSTALL_SH" "$@" >"$OUT" 2>&1
}

fail() {
    echo "    断言失败：$*"
    if [ -s "$OUT" ]; then
        echo "    ---- install.sh 输出 ----"
        sed 's/^/    | /' "$OUT"
    fi
    echo "    ---- 访问日志 ----"
    sed 's/^/    | /' "$ACCESS_LOG"
    exit 1
}

assert_ok() { [ "$1" -eq 0 ] || fail "期望退出码 0，实际 $1"; }
assert_err() { [ "$1" -ne 0 ] || fail "期望非 0 退出码，实际 0"; }
assert_out_has() { grep -F -- "$1" "$OUT" >/dev/null || fail "输出中没有「$1」"; }
assert_out_lacks() { if grep -F -- "$1" "$OUT" >/dev/null; then fail "输出中不应出现「$1」"; fi; }
assert_file() { [ -f "$1" ] || fail "文件不存在：$1"; }
assert_no_file() { [ ! -e "$1" ] || fail "文件不应存在：$1"; }
assert_mode() {
    # shellcheck disable=SC2012 # 只看固定路径的权限位，ls 是 macOS/Linux 都可用的最简方式
    m=$(ls -ld "$1" | cut -c1-10)
    [ "$m" = "$2" ] || fail "$1 权限为 ${m}，期望 $2"
}
assert_file_has() { grep -Fx -- "$2" "$1" >/dev/null || fail "$1 中没有行「$2」"; }
assert_log_has() { grep -F -- "$1" "$ACCESS_LOG" >/dev/null || fail "访问日志中没有「$1」"; }
assert_same() { diff -u "$2" "$1" >/dev/null || fail "$1 与 $2 内容不一致：$(diff -u "$2" "$1")"; }

BIN_PATH=usr/local/bin/minimon-agent
ENV_PATH=etc/minimon/agent.env
UNIT_PATH=etc/systemd/system/minimon-agent.service
PLIST_PATH=Library/LaunchDaemons/com.minimon.agent.plist

# ---------------- 用例 ----------------

case_fresh_install() {
    run_install --server ws://x --token tok
    assert_ok $?
    [ -x "$ROOT/$BIN_PATH" ] || fail "二进制不可执行"
    [ "$("$ROOT/$BIN_PATH")" = "fake 0.1.0" ] || fail "二进制内容不对"
    assert_mode "$ROOT/etc/minimon" drwxr-xr-x
    assert_mode "$ROOT/$ENV_PATH" -rw-------
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_SERVER=ws://x"
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_TOKEN=tok"
    assert_file "$ROOT/$UNIT_PATH"
    assert_same "$ROOT/$UNIT_PATH" "$HERE/minimon-agent.service"
    assert_no_file "$ROOT/$PLIST_PATH"
    assert_log_has "/releases/latest/download/minimon-agent-linux-x86_64"
    assert_log_has "/releases/latest/download/SHA256SUMS"
    assert_out_lacks "tok"
    assert_out_has "ws://x"
    assert_out_has "fake 0.1.0"
    # 不残留临时文件
    leftovers=$(find "$ROOT" -name '.*.new')
    [ -z "$leftovers" ] || fail "残留临时文件：$leftovers"
}

# 用户实际的调用方式：curl … | sudo bash -s -- …（脚本从 stdin 读入）
case_fresh_install_via_bash_stdin() {
    env MINIMON_INSTALL_ROOT="$ROOT" MINIMON_SKIP_SERVICE=1 MINIMON_SKIP_ROOT_CHECK=1 \
        MINIMON_RELEASE_BASE="$BASE_URL/releases" MINIMON_UNAME_S=Linux MINIMON_UNAME_M=amd64 \
        bash -s -- --server wss://example.com/ --token abc123 <"$INSTALL_SH" >"$OUT" 2>&1
    assert_ok $?
    [ "$("$ROOT/$BIN_PATH")" = "fake 0.1.0" ] || fail "二进制内容不对"
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_SERVER=wss://example.com/"
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_TOKEN=abc123"
    assert_same "$ROOT/$UNIT_PATH" "$HERE/minimon-agent.service"
    assert_log_has "/releases/latest/download/minimon-agent-linux-x86_64"
    assert_out_lacks "abc123"
}

case_reinstall_overwrites_and_bad_checksum_keeps_old() {
    run_install --server ws://x --token tok
    assert_ok $?
    run_install --server ws://y --token tok2
    assert_ok $?
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_SERVER=ws://y"
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_TOKEN=tok2"
    if grep -F "MINIMON_TOKEN=tok" "$ROOT/$ENV_PATH" | grep -Fvx "MINIMON_TOKEN=tok2" >/dev/null; then
        fail "旧 token 未被覆盖"
    fi
    assert_mode "$ROOT/$ENV_PATH" -rw-------
    cp "$ROOT/$BIN_PATH" "$WORK/before"
    cp "$ROOT/$ENV_PATH" "$WORK/env-before"
    # 服务端产物被换掉，但 SHA256SUMS 未更新
    printf '#!/bin/sh\necho "tampered"\n' >"$WWW/minimon-agent-linux-x86_64"
    run_install --server ws://z --token tok3
    assert_err $?
    assert_out_has "校验失败"
    assert_same "$ROOT/$BIN_PATH" "$WORK/before"
    assert_same "$ROOT/$ENV_PATH" "$WORK/env-before"
    leftovers=$(find "$ROOT" -name '.*.new')
    [ -z "$leftovers" ] || fail "残留临时文件：$leftovers"
}

case_missing_checksum_entry() {
    : >"$WWW/SHA256SUMS"
    run_install --server ws://x --token tok
    assert_err $?
    assert_out_has "校验失败"
    assert_no_file "$ROOT/$BIN_PATH"
    assert_no_file "$ROOT/$ENV_PATH"
}

case_version_url() {
    run_install --server ws://x --token tok --version 0.2.0
    assert_ok $?
    assert_log_has "/releases/download/v0.2.0/minimon-agent-linux-x86_64"
    assert_log_has "/releases/download/v0.2.0/SHA256SUMS"
    : >"$ACCESS_LOG"
    run_install --server ws://x --token tok --version v0.2.0
    assert_ok $?
    assert_log_has "/releases/download/v0.2.0/minimon-agent-linux-x86_64"
    if grep -F "vv0.2.0" "$ACCESS_LOG" >/dev/null; then fail "版本号出现 vv 前缀"; fi
    if grep -F "/latest/" "$ACCESS_LOG" >/dev/null; then fail "指定版本时不应请求 latest"; fi
}

case_mirror_prefix_join() {
    for m in "$BASE_URL/m" "$BASE_URL/m/"; do
        : >"$ACCESS_LOG"
        run_install --server ws://x --token tok --mirror "$m"
        assert_ok $?
        assert_log_has "/m/$BASE_URL/releases/latest/download/minimon-agent-linux-x86_64"
        assert_log_has "/m/$BASE_URL/releases/latest/download/SHA256SUMS"
        bad=$(grep -v '^/m/http' "$ACCESS_LOG" || true)
        [ -z "$bad" ] || fail "存在未经镜像的请求：$bad"
    done
}

case_missing_args() {
    run_install --server ws://x
    assert_err $?
    assert_out_has "--token"
    assert_no_file "$ROOT/$BIN_PATH"
    run_install --token tok
    assert_err $?
    assert_out_has "--server"
}

case_invalid_values() {
    # shellcheck disable=SC2016 # 故意传入字面量 $(...)
    run_install --server ws://x --token 'tok$(id)'
    assert_err $?
    assert_out_has "--token"
    run_install --server http://x --token tok
    assert_err $?
    assert_out_has "--server"
    run_install --server "ws://x
MINIMON_EVIL=1" --token tok
    assert_err $?
    run_install --server 'ws://x;id' --token tok
    assert_err $?
    assert_no_file "$ROOT/$ENV_PATH"
    run_install --server ws://x --token tok --bogus
    assert_err $?
    assert_out_has "--bogus"
}

case_unknown_arg_hides_value() {
    run_install --server ws://x --tokn=secret123
    assert_err $?
    assert_out_has "--tokn"
    assert_out_lacks "secret123"
    run_install --server ws://x --token tok secret456
    assert_err $?
    assert_out_has "未知参数"
    assert_out_lacks "secret456"
    assert_no_file "$ROOT/$ENV_PATH"
}

# curl | bash 中途断开：脚本在最后一行 main "$@" 之前被截断时不得执行任何操作。
case_truncated_script_runs_nothing() {
    last=$(tail -n 1 "$INSTALL_SH")
    [ "$last" = 'main "$@"' ] || fail "install.sh 最后一行应为 main \"\$@\"，实际：$last"
    n=$(wc -l <"$INSTALL_SH")
    head -n "$((n - 1))" "$INSTALL_SH" >"$WORK/truncated.sh"
    env MINIMON_INSTALL_ROOT="$ROOT" MINIMON_SKIP_SERVICE=1 MINIMON_SKIP_ROOT_CHECK=1 \
        MINIMON_RELEASE_BASE="$BASE_URL/releases" MINIMON_UNAME_S=Linux MINIMON_UNAME_M=x86_64 \
        sh -s -- --server ws://x --token tok <"$WORK/truncated.sh" >"$OUT" 2>&1
    assert_no_file "$ROOT/$BIN_PATH"
    assert_no_file "$ROOT/$ENV_PATH"
    assert_no_file "$ROOT/$UNIT_PATH"
    [ -s "$ACCESS_LOG" ] && fail "截断的脚本不应发起下载"
    [ -s "$OUT" ] && fail "截断的脚本不应有任何输出"
    return 0
}

case_help() {
    run_install --help
    assert_ok $?
    assert_out_has "--server"
    assert_out_has "--uninstall"
    assert_no_file "$ROOT/$BIN_PATH"
}

case_uninstall() {
    run_install --server ws://x --token tok
    assert_ok $?
    assert_file "$ROOT/$BIN_PATH"
    run_install --uninstall
    assert_ok $?
    assert_no_file "$ROOT/$BIN_PATH"
    assert_no_file "$ROOT/$ENV_PATH"
    assert_no_file "$ROOT/$UNIT_PATH"
    assert_no_file "$ROOT/etc/minimon"
    # 再次卸载不报错
    run_install --uninstall
    assert_ok $?
}

case_uninstall_keeps_nonempty_config_dir() {
    run_install --server ws://x --token tok
    assert_ok $?
    : >"$ROOT/etc/minimon/server.env"
    run_install --uninstall
    assert_ok $?
    assert_no_file "$ROOT/$ENV_PATH"
    assert_file "$ROOT/etc/minimon/server.env"
}

case_darwin_plist() {
    UNAME_S=Darwin
    UNAME_M=arm64
    run_install --server wss://x --token sekrit123
    assert_ok $?
    assert_log_has "/releases/latest/download/minimon-agent-darwin-aarch64"
    [ "$("$ROOT/$BIN_PATH")" = "fake 0.1.0" ] || fail "二进制内容不对"
    assert_file "$ROOT/$PLIST_PATH"
    assert_same "$ROOT/$PLIST_PATH" "$HERE/com.minimon.agent.plist"
    if grep -F "sekrit123" "$ROOT/$PLIST_PATH" >/dev/null; then fail "plist 中出现 token"; fi
    assert_no_file "$ROOT/$UNIT_PATH"
    assert_file_has "$ROOT/$ENV_PATH" "MINIMON_TOKEN=sekrit123"
    assert_mode "$ROOT/$ENV_PATH" -rw-------
    assert_out_lacks "sekrit123"
    run_install --uninstall
    assert_ok $?
    assert_no_file "$ROOT/$PLIST_PATH"
    assert_no_file "$ROOT/$BIN_PATH"
}

case_unsupported_platform() {
    UNAME_S=FreeBSD
    run_install --server ws://x --token tok
    assert_err $?
    assert_out_has "不支持的平台"
    UNAME_S=Darwin
    UNAME_M=x86_64
    run_install --server ws://x --token tok
    assert_err $?
    assert_out_has "不支持的平台"
    assert_no_file "$ROOT/$BIN_PATH"
}

case_root_required() {
    if [ "$(id -u)" -eq 0 ]; then
        echo "    （当前为 root，跳过）"
        return 0
    fi
    env MINIMON_INSTALL_ROOT="$ROOT" MINIMON_SKIP_SERVICE=1 \
        MINIMON_RELEASE_BASE="$BASE_URL/releases" MINIMON_UNAME_S=Linux MINIMON_UNAME_M=x86_64 \
        sh "$INSTALL_SH" --server ws://x --token tok >"$OUT" 2>&1
    assert_err $?
    assert_out_has "sudo"
    assert_no_file "$ROOT/$BIN_PATH"
    [ -s "$ACCESS_LOG" ] && fail "非 root 时不应发起下载"
    return 0
}

# ---------------- 运行 ----------------

CASES="fresh_install fresh_install_via_bash_stdin reinstall_overwrites_and_bad_checksum_keeps_old
missing_checksum_entry version_url mirror_prefix_join missing_args invalid_values unknown_arg_hides_value
truncated_script_runs_nothing help uninstall
uninstall_keeps_nonempty_config_dir darwin_plist unsupported_platform root_required"

passed=0
failed=0
for c in $CASES; do
    ROOT="$WORK/root-$c"
    OUT="$WORK/out-$c"
    mkdir -p "$ROOT"
    : >"$OUT"
    : >"$ACCESS_LOG"
    publish 0.1.0
    if (UNAME_S=Linux UNAME_M=x86_64 "case_$c"); then
        echo "PASS $c"
        passed=$((passed + 1))
    else
        echo "FAIL $c"
        failed=$((failed + 1))
    fi
done

echo "结果：$passed 通过，$failed 失败"
[ "$failed" -eq 0 ]
