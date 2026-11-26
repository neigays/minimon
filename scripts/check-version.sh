#!/bin/sh
# 用法：check-version.sh <tag>，校验 tag 与根 Cargo.toml 的 workspace version 一致
set -eu
[ $# -eq 1 ] || { echo "用法：$0 <tag>" >&2; exit 2; }
DIR=$(cd "$(dirname "$0")" && pwd)
TAG=$1
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$DIR/../Cargo.toml" | head -n1)
[ -n "$VERSION" ] || { echo "无法从 Cargo.toml 读取 workspace version" >&2; exit 2; }
if [ "$VERSION" != "${TAG#v}" ]; then
  echo "tag 与版本不一致：tag=${TAG}，Cargo.toml version=${VERSION}" >&2
  exit 1
fi
echo "版本一致：${VERSION}"
