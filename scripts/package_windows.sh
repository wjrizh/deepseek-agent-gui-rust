#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist-win"
APP="力工Studio"
PKG="ligong-studio-rust"
VER="0.1.0"
TARGET="x86_64-pc-windows-msvc"
NODE_VER="${NODE_VER:-22.11.0}"
PW_SRC="${PW_SRC:-$HOME/.ligong-mcp/node_modules}"

PW_REV="$(PW_SRC="$PW_SRC" node -e 'const d=require(process.env.PW_SRC+"/playwright-core/browsers.json");console.log(d.browsers.find(x=>x.name==="chromium").revision)')"
PW_CHR_VER="$(PW_SRC="$PW_SRC" node -e 'const d=require(process.env.PW_SRC+"/playwright-core/browsers.json");console.log(d.browsers.find(x=>x.name==="chromium").browserVersion)')"

echo "==> clean $OUT"
rm -rf "$OUT"
mkdir -p "$OUT"

echo "==> build exe ($TARGET)"
cd "$ROOT"
cargo xwin build --release --target "$TARGET"

STAGE="$OUT/$PKG"
RT="$STAGE/runtime"
mkdir -p "$RT/node_modules" "$RT/browsers" "$RT/bridge"

echo "==> exe"
cp "$ROOT/target/$TARGET/release/deepseek-agent-gui-rust.exe" "$STAGE/$APP.exe"

echo "==> node.exe"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fSL --retry 3 -o "$TMP/node.zip" "https://nodejs.org/dist/v${NODE_VER}/node-v${NODE_VER}-win-x64.zip"
unzip -o -q "$TMP/node.zip" -d "$TMP/node"
find "$TMP/node" -maxdepth 2 -name node.exe -exec cp {} "$RT/node.exe" \;

echo "==> playwright"
cp -r "$PW_SRC/playwright" "$PW_SRC/playwright-core" "$RT/node_modules/"
if [ -d "$PW_SRC/@playwright" ]; then cp -r "$PW_SRC/@playwright" "$RT/node_modules/"; fi

echo "==> chromium $PW_REV ($PW_CHR_VER)"
CFT_URL="https://cdn.playwright.dev/builds/cft/${PW_CHR_VER}/win64/chrome-win64.zip"
curl -fSL --retry 3 -o "$TMP/chrome.zip" "$CFT_URL"
CH="$RT/browsers/chromium-$PW_REV"
mkdir -p "$CH"
unzip -o -q "$TMP/chrome.zip" -d "$CH"
touch "$CH/INSTALLATION_COMPLETE"

echo "==> bridge (Windows 版默认无头；源文件保持有头供 Linux 使用)"
cp "$ROOT/vendor/deepseek-agent/bridge/bridge_server.mjs" "$ROOT/vendor/deepseek-agent/bridge/browser_login.mjs" "$RT/bridge/"
# 仅改打包产物：把 HEADLESS 默认从「有头」改为「无头」（设 LIGONG_HEADLESS=0 可强制有头）。
sed -i "s/const HEADLESS = process.env.LIGONG_HEADLESS === '1';/const HEADLESS = process.env.LIGONG_HEADLESS !== '0';/" "$RT/bridge/bridge_server.mjs"
grep -q "LIGONG_HEADLESS !== '0'" "$RT/bridge/bridge_server.mjs" || { echo "!! bridge 无头替换失败" >&2; exit 1; }

echo "==> seed accounts"
for acc in Y HJ; do
  if [ -d "$HOME/.deepseek-agent/accounts/$acc" ]; then
    mkdir -p "$STAGE/seed/accounts/$acc"
    cp -r "$HOME/.deepseek-agent/accounts/$acc/." "$STAGE/seed/accounts/$acc/" 2>/dev/null || true
  fi
  if [ -f "$HOME/.ligong/accounts/$acc/state.json" ]; then
    mkdir -p "$STAGE/seed/ligong-accounts/$acc"
    cp "$HOME/.ligong/accounts/$acc/state.json" "$STAGE/seed/ligong-accounts/$acc/state.json"
  fi
done

echo "==> zip"
ZIP="$OUT/${PKG}-${VER}-win-x64.zip"
( cd "$OUT" && 7z a -tzip -mx=5 "$ZIP" "$PKG" >/dev/null )
du -h "$ZIP"

echo
echo "==> done"

echo "==> clean intermediate dirs"
rm -rf "$STAGE"

ls -lh "$OUT"/*.zip
