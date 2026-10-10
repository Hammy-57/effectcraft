#!/bin/sh
# Installs (or updates) ~/Applications/EffectCraft.app, the Dock launcher for scripts/fork/app.sh
# (source: scripts/fork/launcher.swift), and adds it to the Dock once. Re-run after changing the
# launcher. macOS only; needs the Xcode command line tools (swiftc).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
app="$HOME/Applications/EffectCraft.app"

mkdir -p "$HOME/Applications"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
swiftc -O -o "$app/Contents/MacOS/EffectCraft" "$root/scripts/fork/launcher.swift" -framework AppKit
cp "$root/assets/app-icon/effectcraft.icns" "$app/Contents/Resources/EffectCraft.icns"
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>EffectCraft</string>
    <key>CFBundleDisplayName</key><string>EffectCraft</string>
    <key>CFBundleIdentifier</key><string>com.eliasx45.effectcraft.launcher</string>
    <key>CFBundleExecutable</key><string>EffectCraft</string>
    <key>CFBundleIconFile</key><string>EffectCraft</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>EffectCraftRoot</key><string>$root</string>
</dict>
</plist>
EOF
codesign --force --sign - "$app" >/dev/null 2>&1 || true
touch "$app"

# Pin it to the Dock once.
if ! defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "Applications/EffectCraft.app"; then
    defaults write com.apple.dock persistent-apps -array-add \
        "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>file://$app/</string><key>_CFURLStringType</key><integer>15</integer></dict></dict></dict>"
    killall Dock
fi
echo "installed $app"
