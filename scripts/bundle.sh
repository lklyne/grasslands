#!/bin/sh
# Build an unsigned release .app bundle: target/bundle/rusted-tools.app
#
#   scripts/bundle.sh            # build and bundle
#   scripts/bundle.sh --no-build # re-bundle the existing release binary
#
# Release builds are NOT socket-guarded (docs/decisions/0007-dev-herdr-guard.md):
# launched plainly, the app connects to your default herdr session and takes
# over its terminals. To try the bundle against a dev session instead, run its
# launcher through the dev environment:
#
#   scripts/dev-run target/bundle/rusted-tools.app/Contents/MacOS/rusted-tools
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/bundle/rusted-tools.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/app/Cargo.toml" | head -n 1)

if [ "${1:-}" != "--no-build" ]; then
  # libghostty needs Zig 0.16 (Homebrew's 0.17 won't build it).
  export ZIG="${ZIG:-$HOME/.local/zig-0.16/zig}"
  [ -x "$ZIG" ] || { echo "bundle: Zig 0.16 not found at $ZIG (set ZIG)" >&2; exit 1; }
  cargo build --release -p app --manifest-path "$root/Cargo.toml"
fi

bin="$root/target/release/app"
[ -x "$bin" ] || { echo "bundle: $bin missing; run without --no-build" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/rusted-tools-bin"

# Apps started from Finder or the Dock get launchd's minimal PATH, so `herdr`
# from Homebrew or ~/.local/bin wouldn't be found. The launcher adds the usual
# install locations; HERDR_BIN, HERDR_SESSION and HERDR_SOCKET_PATH pass through.
cat > "$app/Contents/MacOS/rusted-tools" <<'EOF'
#!/bin/sh
PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
export PATH
exec "$(dirname "$0")/rusted-tools-bin" "$@"
EOF
chmod +x "$app/Contents/MacOS/rusted-tools"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>rusted-tools</string>
  <key>CFBundleDisplayName</key><string>rusted-tools</string>
  <key>CFBundleIdentifier</key><string>dev.lklyne.rusted-tools</string>
  <key>CFBundleExecutable</key><string>rusted-tools</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF

plutil -lint "$app/Contents/Info.plist" >/dev/null
echo "bundle: $app"
