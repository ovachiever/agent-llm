#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PRODUCT_NAME="agent-llm"
EXECUTABLE_NAME="AgentLlmMac"
APP_DIR="$ROOT/release/$PRODUCT_NAME.app"
ICON_SOURCE="$ROOT/../desktop/build/icon.icns"
CACHE_DIR="$ROOT/.build/module-cache"
CLANG_CACHE_DIR="$ROOT/.build/clang-module-cache"

env \
  SWIFTPM_MODULECACHE_OVERRIDE="$CACHE_DIR" \
  CLANG_MODULE_CACHE_PATH="$CLANG_CACHE_DIR" \
  swift build --package-path "$ROOT" -c release

EXECUTABLE_PATH="$(find "$ROOT/.build" -type f -path "*/release/$EXECUTABLE_NAME" | head -n 1)"
if [[ -z "$EXECUTABLE_PATH" ]]; then
  echo "Failed to locate built executable for $EXECUTABLE_NAME" >&2
  exit 1
fi

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cp "$EXECUTABLE_PATH" "$APP_DIR/Contents/MacOS/$EXECUTABLE_NAME"
cp "$ICON_SOURCE" "$APP_DIR/Contents/Resources/AppIcon.icns"

cat > "$APP_DIR/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>agent-llm</string>
  <key>CFBundleExecutable</key>
  <string>AgentLlmMac</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>com.ovachiever.agentllm.macos</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>agent-llm</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSAppTransportSecurity</key>
  <dict>
    <key>NSAllowsLocalNetworking</key>
    <true/>
  </dict>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

touch "$APP_DIR"
echo "Built $APP_DIR"
