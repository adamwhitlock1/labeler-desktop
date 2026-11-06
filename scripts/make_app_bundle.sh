#!/usr/bin/env bash
# Wraps a cargo binary in a minimal .app bundle with the Info.plist keys
# macOS requires for Bluetooth access (CoreBluetooth aborts without them
# on an unbundled executable). Dev/test convenience only.
set -euo pipefail
BIN_NAME="$1"           # e.g. label-cli
BIN_PATH="$2"           # e.g. target/debug/label-cli
APP_NAME="${3:-$BIN_NAME}"
OUT_DIR="dist/${APP_NAME}.app"

mkdir -p "$OUT_DIR/Contents/MacOS"
cp "$BIN_PATH" "$OUT_DIR/Contents/MacOS/$BIN_NAME"

cat > "$OUT_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>${BIN_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>dev.local.${APP_NAME}</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSBluetoothAlwaysUsageDescription</key>
    <string>Used to discover and print to your DYMO LetraTag 200B label maker.</string>
    <key>NSBluetoothPeripheralUsageDescription</key>
    <string>Used to discover and print to your DYMO LetraTag 200B label maker.</string>
</dict>
</plist>
PLIST

echo "built $OUT_DIR"
