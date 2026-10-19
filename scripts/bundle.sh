#!/bin/sh
# Build a release .app bundle, signed ad hoc: target/bundle/grasslands.app.
# scripts/release.sh signs it with the Developer ID and notarizes it.
#
#   scripts/bundle.sh            # build and bundle
#   scripts/bundle.sh --no-build # re-bundle the existing release binary
#
# Release builds are NOT socket-guarded (docs/decisions/0007-dev-herdr-guard.md):
# launched plainly, the app connects to your default herdr session and takes
# over its terminals. To try the bundle against a dev session instead, run its
# binary through the dev environment:
#
#   scripts/dev-run target/bundle/grasslands.app/Contents/MacOS/grasslands
#
# The bundle carries Sparkle.framework (scripts/fetch-sparkle.sh), the
# updater, and names its feed and public key in Info.plist; see
# docs/decisions/0024-updates.md. SPARKLE_PUBLIC_KEY overrides the key, for
# trying a bundle before UPDATE_PUBLIC_KEY is set; with neither, the bundle
# has no updater.
set -eu

# Where updates are announced. `latest` follows the newest regular (not
# pre-) release, so the feed's address never changes.
FEED_URL="https://github.com/lklyne/grasslands/releases/latest/download/appcast.xml"
# The public half of the EdDSA key updates are signed with. The private half
# is in the maintainer's login keychain under the account `grasslands`.
UPDATE_PUBLIC_KEY=""

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/bundle/grasslands.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/app/Cargo.toml" | head -n 1)
public_key="${SPARKLE_PUBLIC_KEY:-$UPDATE_PUBLIC_KEY}"

if [ "${1:-}" != "--no-build" ]; then
  # libghostty needs Zig 0.16 (Homebrew's 0.17 won't build it).
  export ZIG="${ZIG:-$HOME/.local/zig-0.16/zig}"
  [ -x "$ZIG" ] || { echo "bundle: Zig 0.16 not found at $ZIG (set ZIG)" >&2; exit 1; }
  cargo build --release -p app --manifest-path "$root/Cargo.toml"
fi

bin="$root/target/release/grasslands"
[ -x "$bin" ] || { echo "bundle: $bin missing; run without --no-build" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/grasslands"
sparkle=$("$root/scripts/fetch-sparkle.sh")
if [ -n "$public_key" ]; then
  mkdir -p "$app/Contents/Frameworks"
  cp -R "$sparkle/Sparkle.framework" "$app/Contents/Frameworks/"
  updater_keys="  <key>SUFeedURL</key><string>$FEED_URL</string>
  <key>SUPublicEDKey</key><string>$public_key</string>
  <key>SUEnableAutomaticChecks</key><true/>
  <key>SUScheduledCheckInterval</key><integer>86400</integer>"
else
  echo "bundle: no UPDATE_PUBLIC_KEY or SPARKLE_PUBLIC_KEY; this bundle has no updater" >&2
  updater_keys=""
fi
"$root/scripts/compile-icon.sh" "$app/Contents/Resources"

# A binary release has to carry its dependencies' notices. cargo-about covers
# the crates; Ghostty (built from Zig source) and cuelume (ported, not a
# dependency) come from files. Install it with
# `cargo install cargo-about --locked --features cli`.
command -v cargo-about >/dev/null || { echo "bundle: cargo-about missing" >&2; exit 1; }
notices="$app/Contents/Resources/THIRD-PARTY-NOTICES.txt"
cargo about generate --locked -c "$root/packaging/about.toml" -m "$root/crates/app/Cargo.toml" \
  "$root/packaging/about.hbs" -o "$notices"
for extra in "Ghostty:$root/vendor/gpui-libghostty/vendor/ghostty/LICENSE" \
  "gpui-libghostty:$root/packaging/notices/gpui-libghostty.txt" \
  "cuelume (ported in crates/cues):$root/packaging/notices/cuelume.txt" \
  "Sparkle:$root/packaging/notices/sparkle.txt"; do
  printf '\n================================================================================\n%s\n\n' "${extra%%:*}" >>"$notices"
  cat "${extra#*:}" >>"$notices"
done

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>grasslands</string>
  <key>CFBundleDisplayName</key><string>grasslands</string>
  <key>CFBundleIdentifier</key><string>dev.lklyne.grasslands</string>
  <key>CFBundleExecutable</key><string>grasslands</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIconName</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
$updater_keys
</dict>
</plist>
EOF

plutil -lint "$app/Contents/Info.plist" >/dev/null
"$root/scripts/sign-bundle.sh" "$app" >/dev/null 2>&1 \
  || { echo "bundle: ad hoc signing failed" >&2; exit 1; }
echo "bundle: $app"
