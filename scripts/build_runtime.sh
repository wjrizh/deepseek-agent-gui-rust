#!/usr/bin/env bash
# 打包便携浏览器运行时到 dist/runtime/：
#   runtime/node.exe               Windows 版 Node（供目标机直接执行，无需安装）
#   runtime/node_modules/...       @playwright/mcp 及其依赖
#   runtime/package.json           npm 元数据（保留）
#   runtime/VERSION                版本记录
#
# 用法: scripts/build_runtime.sh [node_version]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist/runtime"
NODE_VER="${1:-22.11.0}"
NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmjs.org}"

echo "==> 目标目录: $OUT"
rm -rf "$OUT"
mkdir -p "$OUT"

# 1) 下载 Windows Node（zip 内含 node.exe）
ZIP_URL="https://nodejs.org/dist/v${NODE_VER}/node-v${NODE_VER}-win-x64.zip"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "==> 下载 Node: $ZIP_URL"
curl -fSL --retry 3 -o "$TMP/node.zip" "$ZIP_URL"
echo "==> 解压 node.exe"
unzip -o -q "$TMP/node.zip" -d "$TMP/node"
NODE_EXE="$(find "$TMP/node" -maxdepth 2 -name node.exe | head -1)"
if [[ -z "$NODE_EXE" ]]; then echo "未找到 node.exe" >&2; exit 1; fi
cp "$NODE_EXE" "$OUT/node.exe"
echo "   node.exe: $(du -h "$OUT/node.exe" | cut -f1)"

# 2) 安装 @playwright/mcp（纯 JS，跨平台通用）
echo "==> 安装 @playwright/mcp"
STAGE="$TMP/stage"
mkdir -p "$STAGE"
cd "$STAGE"
printf '{"name":"ligong-browser-runtime","private":true}\n' > package.json
npm install --registry="$NPM_REGISTRY" --no-audit --no-fund --omit=dev @playwright/mcp >/dev/null

CLI="$STAGE/node_modules/@playwright/mcp/cli.js"
if [[ ! -f "$CLI" ]]; then echo "未找到 cli.js" >&2; exit 1; fi

echo "==> 复制 node_modules"
cp -r "$STAGE/node_modules" "$OUT/node_modules"
cp "$STAGE/package.json" "$OUT/package.json"

# 2.5) 下载 Windows Chromium（Chrome for Testing），组装为 Playwright 约定布局
BROWSERS_JSON="$OUT/node_modules/playwright-core/browsers.json"
CHROMIUM_REV="$(node -e "const d=require('$BROWSERS_JSON');const b=d.browsers.find(x=>x.name==='chromium');console.log(b.revision)")"
CHROMIUM_VER="$(node -e "const d=require('$BROWSERS_JSON');const b=d.browsers.find(x=>x.name==='chromium');console.log(b.browserVersion)")"
if [[ -z "$CHROMIUM_REV" || -z "$CHROMIUM_VER" ]]; then echo "无法从 browsers.json 读取 chromium 版本" >&2; exit 1; fi
echo "==> Chromium revision=$CHROMIUM_REV version=$CHROMIUM_VER"

if [[ "${SKIP_CHROMIUM:-0}" != "1" ]]; then
  CFT_URL="${CFT_BASE:-https://cdn.playwright.dev/builds/cft}/${CHROMIUM_VER}/win64/chrome-win64.zip"
  echo "==> 下载 Chromium: $CFT_URL"
  curl -fSL --retry 3 -o "$TMP/chrome-win64.zip" "$CFT_URL"
  CH_DIR="$OUT/browsers/chromium-${CHROMIUM_REV}"
  mkdir -p "$CH_DIR"
  unzip -o -q "$TMP/chrome-win64.zip" -d "$CH_DIR"
  touch "$CH_DIR/INSTALLATION_COMPLETE" "$CH_DIR/DEPENDENCIES_VALIDATED"
  echo "   chromium: $CH_DIR/chrome-win64/chrome.exe"
else
  echo "==> 跳过 Chromium（SKIP_CHROMIUM=1）"
fi

# 3) 版本记录
{
  echo "node=${NODE_VER}"
  echo "mcp=$(node -e "console.log(require('$CLI/../../../@playwright/mcp/package.json').version)" 2>/dev/null || echo unknown)"
  echo "chromium=${CHROMIUM_REV} (${CHROMIUM_VER})"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$OUT/VERSION"

echo "==> 完成。体积:"
du -sh "$OUT"
echo "    cli.js: $OUT/node_modules/@playwright/mcp/cli.js"
