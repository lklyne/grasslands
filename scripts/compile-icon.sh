#!/bin/sh
# Compile crates/app/assets/AppIcon.icon, an Icon Composer document, into a
# bundle's Resources: Assets.car, which macOS 26 draws as layered glass, and a
# flat AppIcon.icns for older systems. Info.plist names both `AppIcon`.
#
#   scripts/compile-icon.sh RESOURCES_DIR
#
# An actool too old to read the document falls back to the copy committed in
# crates/app/assets/AppIcon.compiled. After changing the icon, refresh that
# copy with UPDATE_COMPILED_ICON=1 and commit it.
set -eu

resources="$1"
assets="$(cd "$(dirname "$0")/../crates/app/assets" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

xcrun actool "$assets/AppIcon.icon" --compile "$tmp" --app-icon AppIcon \
  --platform macosx --minimum-deployment-target 13.0 \
  --output-partial-info-plist "$tmp/partial.plist" >"$tmp/actool.log" 2>&1 || true
if [ ! -f "$tmp/Assets.car" ] || [ ! -f "$tmp/AppIcon.icns" ]; then
  echo "compile-icon: actool could not compile the icon; using AppIcon.compiled" >&2
  cp "$assets/AppIcon.compiled/Assets.car" "$assets/AppIcon.compiled/AppIcon.icns" "$resources/"
  exit 0
fi
cp "$tmp/Assets.car" "$tmp/AppIcon.icns" "$resources/"
if [ "${UPDATE_COMPILED_ICON:-0}" = 1 ]; then
  cp "$tmp/Assets.car" "$tmp/AppIcon.icns" "$assets/AppIcon.compiled/"
fi
