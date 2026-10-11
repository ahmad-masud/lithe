#!/usr/bin/env bash
# Builds lithe.app, a proper macOS application bundle.
#
# A bare Mach-O binary run from a terminal is only half an app to macOS: the
# menu bar is unreliable, the Dock shows the executable name, and there is
# no icon. An Info.plist is what makes it a real one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app="$root/target/lithe.app"
version="$(awk -F'"' '/^version/ {print $2; exit}' "$root/Cargo.toml")"

cargo build --release --manifest-path "$root/Cargo.toml"

# The icon is generated rather than committed as a binary blob.
if [[ ! -f "$root/assets/lithe.icns" ]]; then
  python3 "$root/scripts/make-icon.py" >/dev/null
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/target/release/lithe" "$app/Contents/MacOS/lithe"
cp "$root/assets/lithe.icns" "$app/Contents/Resources/lithe.icns"
printf 'APPL????' > "$app/Contents/PkgInfo"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>                  <string>lithe</string>
  <key>CFBundleDisplayName</key>           <string>lithe</string>
  <key>CFBundleExecutable</key>            <string>lithe</string>
  <key>CFBundleIdentifier</key>            <string>dev.lithe.editor</string>
  <key>CFBundleIconFile</key>              <string>lithe</string>
  <key>CFBundlePackageType</key>           <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key> <string>6.0</string>
  <key>CFBundleShortVersionString</key>    <string>${version}</string>
  <key>CFBundleVersion</key>               <string>${version}</string>
  <key>LSMinimumSystemVersion</key>        <string>11.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key>       <true/>
  <!-- No CFBundleDocumentTypes yet: declaring them would put lithe in
       "Open With" menus, and opening a file that way sends an
       application:openFiles: event the editor does not handle. That is a
       promise to keep once it does. -->
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature. Unsigned bundles get inconsistent treatment from
# Gatekeeper and the Dock; this costs nothing and makes behaviour stable.
codesign --force --sign - "$app" 2>/dev/null || \
  echo "note: ad-hoc codesign failed; the app still runs" >&2

# So Finder and the Dock notice the new bundle rather than a cached one.
touch "$app"

echo "$app"
