#!/usr/bin/env bash
# html2pdf —— 用无头 Chromium/Chrome 把 HTML 渲染成 PDF（与项目设计文档的生成方式一致）。
#
# 用法:
#   html2pdf <输入.html> [输出.pdf]
# 省略输出时，输出与输入同名的 .pdf。
#
# 依赖: 本机已安装 Chromium 系浏览器（chromium / chromium-browser / google-chrome）。
# 注意: snap 版 chromium 有私有 /tmp，输出 PDF 请放在 home 目录下，不要放 /tmp。
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  echo "用法: html2pdf <输入.html> [输出.pdf]" >&2
  exit 1
fi

in="$1"
out="${2:-${in%.*}.pdf}"

if [ ! -f "$in" ]; then
  echo "错误: 找不到输入文件: $in" >&2
  exit 1
fi

# 找一个可用的 Chromium 系浏览器
BROWSER=""
for c in chromium chromium-browser google-chrome google-chrome-stable; do
  if command -v "$c" >/dev/null 2>&1; then
    BROWSER="$(command -v "$c")"
    break
  fi
done
if [ -z "$BROWSER" ]; then
  echo "错误: 未找到 Chromium/Chrome（已尝试 chromium / chromium-browser / google-chrome）" >&2
  exit 1
fi

in_abs="$(realpath "$in")"
out_abs="$(realpath -m "$out")"

"$BROWSER" --headless --no-sandbox --disable-gpu --no-pdf-header-footer \
  --print-to-pdf="$out_abs" "file://$in_abs" >/dev/null 2>&1

if [ -f "$out_abs" ]; then
  echo "已生成: $out_abs"
else
  echo "错误: PDF 生成失败" >&2
  exit 1
fi
