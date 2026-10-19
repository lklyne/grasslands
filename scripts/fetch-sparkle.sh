#!/bin/sh
# Downloads the Sparkle distribution the app's updater comes from, checks it
# against the pinned hash and prints the folder it is in. Does nothing when
# it is already there. From Specular (github.com/lklyne/specular,
# native/scripts/fetch-sparkle.sh), which shares the cache.
#
# The folder holds `Sparkle.framework`, which the bundle embeds,
# `bin/sign_update`, which signs an update archive for the feed, and
# `bin/generate_keys`, which made the signing key (docs/decisions/0024).
#
#   scripts/fetch-sparkle.sh
#   SPARKLE_PATH  where to keep it (default ~/.local/share/sparkle/<version>)
set -eu

VERSION="2.10.0"
SHA256="c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c"

dir="${SPARKLE_PATH:-$HOME/.local/share/sparkle/$VERSION}"
if [ ! -d "$dir/Sparkle.framework" ]; then
  tmp=$(mktemp -d)
  archive="$tmp/Sparkle-$VERSION.tar.xz"
  curl -fsSL -o "$archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$VERSION/Sparkle-$VERSION.tar.xz"
  echo "$SHA256  $archive" | shasum -a 256 -c - >/dev/null \
    || { echo "fetch-sparkle: Sparkle $VERSION does not match its pinned hash" >&2; exit 1; }
  mkdir -p "$dir"
  tar -xf "$archive" -C "$dir"
  rm -rf "$tmp"
fi
echo "$dir"
