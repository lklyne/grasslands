#!/bin/sh
# Build, sign, notarize and zip a release, and write its update feed:
#
#   target/release-zip/grasslands-<version>-arm64.zip   the app, for people,
#                                                       Homebrew and Sparkle
#   target/release-zip/appcast.xml                      the feed that names it
#
#   scripts/release.sh
#
# Needs a "Developer ID Application" identity in the keychain and notarytool
# credentials stored under a keychain profile:
#
#   xcrun notarytool store-credentials grasslands-notary \
#     --apple-id <apple id> --team-id <team id> --password <app-specific password>
#
# and Sparkle's EdDSA private key: SPARKLE_PRIVATE_KEY (the key itself), or
# the login keychain under the account `grasslands` (`generate_keys
# --account grasslands`, see docs/decisions/0024-updates.md). bundle.sh's
# UPDATE_PUBLIC_KEY must be its public half.
#
# SIGN_IDENTITY and NOTARY_PROFILE override the defaults below.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/bundle/grasslands.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/app/Cargo.toml" | head -n 1)
identity="${SIGN_IDENTITY:-Developer ID Application}"
profile="${NOTARY_PROFILE:-grasslands-notary}"
releases="https://github.com/lklyne/grasslands/releases"
out="$root/target/release-zip"
zip="$out/grasslands-$version-arm64.zip"
feed="$out/appcast.xml"

"$root/scripts/bundle.sh"
[ -d "$app/Contents/Frameworks/Sparkle.framework" ] \
  || { echo "release: the bundle has no updater; set UPDATE_PUBLIC_KEY in scripts/bundle.sh" >&2; exit 1; }
sparkle=$("$root/scripts/fetch-sparkle.sh")

"$root/scripts/sign-bundle.sh" "$app" "$identity"

mkdir -p "$out"
rm -f "$zip" "$feed"
# notarytool takes a zip; staple the app afterwards and re-zip it so the
# ticket ships inside the bundle
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" --keychain-profile "$profile" --wait
xcrun stapler staple "$app"
spctl --assess --type execute --verbose=2 "$app"

rm -f "$zip"
# --sequesterRsrc keeps extended attributes out of the app, as Sparkle asks.
ditto -c -k --sequesterRsrc --keepParent "$app" "$zip"

# `sign_update` prints the enclosure's two attributes: the archive's
# signature and its length.
if [ -n "${SPARKLE_PRIVATE_KEY:-}" ]; then
  signature=$(printf '%s' "$SPARKLE_PRIVATE_KEY" | "$sparkle/bin/sign_update" --ed-key-file - "$zip")
else
  signature=$("$sparkle/bin/sign_update" --account grasslands "$zip")
fi
cat > "$feed" <<FEED
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>grasslands</title>
    <item>
      <title>grasslands $version</title>
      <pubDate>$(date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
      <sparkle:fullReleaseNotesLink>$releases/tag/v$version</sparkle:fullReleaseNotesLink>
      <enclosure url="$releases/download/v$version/grasslands-$version-arm64.zip" type="application/octet-stream" $signature/>
    </item>
  </channel>
</rss>
FEED
xmllint --noout "$feed"

echo "release: $zip"
echo "release: $feed"
shasum -a 256 "$zip"
