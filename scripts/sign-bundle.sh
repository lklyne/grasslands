#!/bin/sh
# Sign a grasslands.app inside out: Sparkle's helpers, Sparkle.framework,
# then the app, in the order Sparkle's docs give (sparkle-project.org,
# "Code Signing"). `codesign --deep` would sign the nested code with the
# app's options and drop the downloader's entitlements, so each part is
# signed on its own.
#
#   scripts/sign-bundle.sh APP            # ad hoc: runs on this Mac only
#   scripts/sign-bundle.sh APP IDENTITY   # Developer ID, hardened runtime
#                                         # and a timestamp, for notarizing
set -eu

app="$1"
identity="${2:-}"

sign() {
  if [ -z "$identity" ]; then
    codesign --force --sign - "$@"
  else
    codesign --force --sign "$identity" --options runtime --timestamp "$@"
  fi
}

sparkle="$app/Contents/Frameworks/Sparkle.framework"
if [ -d "$sparkle" ]; then
  sign "$sparkle/Versions/B/XPCServices/Installer.xpc"
  # The downloader keeps the entitlements it shipped with.
  sign --preserve-metadata=entitlements "$sparkle/Versions/B/XPCServices/Downloader.xpc"
  sign "$sparkle/Versions/B/Autoupdate"
  sign "$sparkle/Versions/B/Updater.app"
  sign "$sparkle"
fi
sign "$app"
codesign --verify --deep --strict --verbose=2 "$app"
