#!/bin/sh
# scripts/check-version.sh 与 scripts/check-size.sh 的测试
set -u
DIR=$(cd "$(dirname "$0")" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
FAIL=0

ok() { echo "PASS: $1"; }
bad() { echo "FAIL: $1"; FAIL=1; }

# 与根 Cargo.toml 一致的 tag 应通过
V=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$DIR/../Cargo.toml" | head -n1)
if sh "$DIR/check-version.sh" "v$V" >/dev/null 2>&1; then ok "check-version 匹配 v$V"; else bad "check-version 匹配 v$V"; fi

OUT=$(sh "$DIR/check-version.sh" v9.9.9 2>&1); RC=$?
if [ "$RC" -eq 1 ] && echo "$OUT" | grep -q "不一致"; then ok "check-version 不匹配"; else bad "check-version 不匹配 (rc=$RC)"; fi

printf '0123456789' > "$TMP/f"
OUT=$(sh "$DIR/check-size.sh" "$TMP/f" 5 2>&1); RC=$?
if [ "$RC" -eq 1 ] && echo "$OUT" | grep -q "$TMP/f"; then ok "check-size 超限"; else bad "check-size 超限 (rc=$RC)"; fi
if sh "$DIR/check-size.sh" "$TMP/f" 100 >/dev/null 2>&1; then ok "check-size 未超限"; else bad "check-size 未超限"; fi

exit "$FAIL"
