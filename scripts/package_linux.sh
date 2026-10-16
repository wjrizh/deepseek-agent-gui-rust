#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist-linux"
APP="力工Studio-Rust"
PKG="ligong-studio-rust"
VER="0.1.0"
ARCH="amd64"
PW_SRC="${PW_SRC:-$HOME/.ligong-mcp/node_modules}"
NODE_VER="${NODE_VER:-22.11.0}"
NODE_SRC="${NODE_SRC:-}"

PW_REV="$(PW_SRC="$PW_SRC" node -e 'const d=require(process.env.PW_SRC+"/playwright-core/browsers.json");console.log(d.browsers.find(x=>x.name==="chromium").revision)')"
PW_CHR_VER="$(PW_SRC="$PW_SRC" node -e 'const d=require(process.env.PW_SRC+"/playwright-core/browsers.json");console.log(d.browsers.find(x=>x.name==="chromium").browserVersion)')"

echo "==> clean $OUT"
rm -rf "$OUT"
mkdir -p "$OUT"

echo "==> cargo build --release"
cd "$ROOT"
cargo build --release

STAGE="$OUT/stage"
ROOTDIR="$STAGE/opt/$PKG"
mkdir -p "$ROOTDIR/runtime/node_modules" "$ROOTDIR/runtime/browsers" "$ROOTDIR/runtime/bridge"

echo "==> binary"
cp "$ROOT/target/release/deepseek-agent-gui-rust" "$ROOTDIR/$APP"
chmod +x "$ROOTDIR/$APP"

echo "==> node"
if [ -n "$NODE_SRC" ] && [ -x "$NODE_SRC" ]; then cp "$NODE_SRC" "$ROOTDIR/runtime/node"; else NODE_TMP="$(mktemp -d)"; curl -fSL --retry 3 -o "$NODE_TMP/node.tar.xz" "https://nodejs.org/dist/v${NODE_VER}/node-v${NODE_VER}-linux-x64.tar.xz"; tar -xJf "$NODE_TMP/node.tar.xz" -C "$NODE_TMP"; cp "$NODE_TMP/node-v${NODE_VER}-linux-x64/bin/node" "$ROOTDIR/runtime/node"; rm -rf "$NODE_TMP"; fi
chmod +x "$ROOTDIR/runtime/node"
if ldd "$ROOTDIR/runtime/node" 2>/dev/null | grep -qi "not found\|libada"; then echo "!! 打包的 node 依赖异常(not found/libada)，请设 NODE_SRC 指向官方 node" >&2; ldd "$ROOTDIR/runtime/node" >&2 || true; exit 1; fi

echo "==> playwright"
cp -r "$PW_SRC/playwright" "$PW_SRC/playwright-core" "$ROOTDIR/runtime/node_modules/"
if [ -d "$PW_SRC/@playwright" ]; then cp -r "$PW_SRC/@playwright" "$ROOTDIR/runtime/node_modules/"; fi

echo "==> chromium $PW_REV ($PW_CHR_VER)"
SRC_CHR=""
for c in "$HOME/.cache/ms-playwright/chromium-$PW_REV/chrome-linux64" "$HOME/.cache/ms-playwright/chromium-$PW_REV/chrome-linux"; do
  if [ -d "$c" ]; then SRC_CHR="$c"; break; fi
done
if [ -z "$SRC_CHR" ]; then
  echo "!! chromium-$PW_REV not found" >&2
else
  DST_CHR="$ROOTDIR/runtime/browsers/chromium-$PW_REV/chrome-linux64"
  mkdir -p "$DST_CHR"
  cp -r "$SRC_CHR/." "$DST_CHR/"
  touch "$ROOTDIR/runtime/browsers/chromium-$PW_REV/INSTALLATION_COMPLETE"
fi

echo "==> bridge"
cp "$ROOT/vendor/deepseek-agent/bridge/bridge_server.mjs" "$ROOT/vendor/deepseek-agent/bridge/browser_login.mjs" "$ROOTDIR/runtime/bridge/"

# 打包登录态：cookie(state.json) + 持久化浏览器 profile。
# 产物运行时会用 run.sh 将 HOME 指向 $ROOTDIR/data，故按 ~/.ligong 与 ~/.deepseek-agent 结构放置。
echo "==> login state (cookies + profiles)"
DATADIR="$ROOTDIR/data"
mkdir -p "$DATADIR/.ligong/accounts" "$DATADIR/.deepseek-agent/accounts"
SRC_HOME="${SRC_HOME:-$HOME}"
if [ -d "$SRC_HOME/.ligong/accounts" ]; then
  cp -r "$SRC_HOME/.ligong/accounts/." "$DATADIR/.ligong/accounts/"
fi
if [ -d "$SRC_HOME/.deepseek-agent/accounts" ]; then
  cp -r "$SRC_HOME/.deepseek-agent/accounts/." "$DATADIR/.deepseek-agent/accounts/"
fi
echo "   accounts: $(ls "$DATADIR/.ligong/accounts" 2>/dev/null | tr '\n' ' ')"

echo "==> run.sh"
cat > "$ROOTDIR/run.sh" <<'RUNEOF'
#!/usr/bin/env bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# 登录态与浏览器 profile 随包分发，指向产物内 data 目录（应用按 $HOME 读取）。
export HOME="$DIR/data"
export LIGONG_RUNTIME="$DIR/runtime"
export LIGONG_NODE="$DIR/runtime/node"
export LIGONG_PLAYWRIGHT="$DIR/runtime/node_modules/playwright"
export LIGONG_BRIDGE_DIR="$DIR/runtime/bridge"
# 打包产物默认有头运行（需要可见的浏览器窗口）；无头可设 LIGONG_HEADLESS=1 ./run.sh
export LIGONG_HEADLESS=0
CHR="$(find "$DIR/runtime/browsers" -maxdepth 3 -name chrome -type f 2>/dev/null | head -1)"
if [ -n "$CHR" ]; then export LIGONG_CHROMIUM="$CHR"; fi
exec "$DIR/力工Studio-Rust" "$@"
RUNEOF
chmod +x "$ROOTDIR/run.sh"

echo "==> tar.gz"
TARBALL="$OUT/${PKG}-${VER}-linux-x64.tar.gz"
tar -C "$STAGE/opt" -czf "$TARBALL" "$PKG"
du -h "$TARBALL"

echo "==> deb"
DEBROOT="$OUT/debroot"
rm -rf "$DEBROOT"
mkdir -p "$DEBROOT/DEBIAN" "$DEBROOT/opt" "$DEBROOT/usr/bin" "$DEBROOT/usr/share/applications"
cp -r "$STAGE/opt/$PKG" "$DEBROOT/opt/"
ln -sf "/opt/$PKG/run.sh" "$DEBROOT/usr/bin/$PKG"
chmod +x "$DEBROOT/opt/$PKG/run.sh"

printf 'Package: %s\nVersion: %s\nSection: utils\nPriority: optional\nArchitecture: %s\nMaintainer: ligong\nDepends: libgtk-3-0 | libgtk-3-0t64, libwebkit2gtk-4.1-0, libnotify4\nRecommends: curl, unzip, xvfb, xauth, alacritty\nDescription: 力工Studio-Rust - DeepSeek browser automation desktop client\n' "$PKG" "$VER" "$ARCH" > "$DEBROOT/DEBIAN/control"

printf '[Desktop Entry]\nName=力工Studio-Rust\nExec=/opt/%s/run.sh\nIcon=/opt/%s/icon.png\nType=Application\nCategories=Utility;\nTerminal=false\n' "$PKG" "$PKG" > "$DEBROOT/usr/share/applications/$PKG.desktop"

if [ -f "$ROOT/icons/icon.png" ]; then cp "$ROOT/icons/icon.png" "$DEBROOT/opt/$PKG/icon.png"; fi

echo "2.0" > "$OUT/debian-binary"
( cd "$DEBROOT/DEBIAN" && tar -czf "$OUT/control.tar.gz" . )
( cd "$DEBROOT" && tar --exclude=./DEBIAN -czf "$OUT/data.tar.gz" . )
DEB="$OUT/${PKG}_${VER}_${ARCH}.deb"
rm -f "$DEB"
( cd "$OUT" && ar rcs "$DEB" debian-binary control.tar.gz data.tar.gz )
rm -f "$OUT/debian-binary" "$OUT/control.tar.gz" "$OUT/data.tar.gz"

echo
echo "==> done"

echo "==> clean intermediate dirs"
rm -rf "$STAGE" "$DEBROOT"

ls -lh "$OUT"/*.tar.gz "$OUT"/*.deb
