#!/bin/sh
# Build, sign, notarize and zip a release: target/release-zip/grasslands-<version>-arm64.zip
#
#   scripts/release.sh
#
# Needs a "Developer ID Application" identity in the keychain and notarytool
# credentials stored under a keychain profile:
#
#   xcrun notarytool store-credentials grasslands-notary \
#     --apple-id <apple id> --team-id <team id> --password <app-specific password>
#
# SIGN_IDENTITY and NOTARY_PROFILE override the defaults below.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/bundle/grasslands.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/app/Cargo.toml" | head -n 1)
identity="${SIGN_IDENTITY:-Developer ID Application}"
profile="${NOTARY_PROFILE:-grasslands-notary}"
out="$root/target/release-zip"
zip="$out/grasslands-$version-arm64.zip"

"$root/scripts/bundle.sh"

codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

mkdir -p "$out"
rm -f "$zip"
# notarytool takes a zip; staple the app afterwards and re-zip it so the
# ticket ships inside the bundle
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" --keychain-profile "$profile" --wait
xcrun stapler staple "$app"
spctl --assess --type execute --verbose=2 "$app"

rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "release: $zip"
shasum -a 256 "$zip"
