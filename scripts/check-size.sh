#!/bin/sh
# 用法：check-size.sh <file> <max_bytes>，文件超出上限时退出 1
set -eu
[ $# -eq 2 ] || { echo "用法：$0 <file> <max_bytes>" >&2; exit 2; }
FILE=$1
MAX=$2
[ -f "$FILE" ] || { echo "文件不存在：${FILE}" >&2; exit 2; }
SIZE=$(wc -c < "$FILE" | tr -d ' ')
if [ "$SIZE" -gt "$MAX" ]; then
  echo "体积超限：${FILE} 大小 ${SIZE} 字节，上限 ${MAX} 字节" >&2
  exit 1
fi
echo "体积正常：${FILE} ${SIZE} / ${MAX} 字节"
