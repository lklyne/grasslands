# Homebrew cask

`grasslands.rb` is the cask in [lklyne/homebrew-tap](https://github.com/lklyne/homebrew-tap) (`Casks/grasslands.rb`), so people can run `brew install --cask lklyne/tap/grasslands`. This copy is the template; the tap holds the live `version` and `sha256`.

## Cutting a release

1. Bump `version` in `crates/app/Cargo.toml`. `scripts/bundle.sh` writes it into `Info.plist`.
2. Build, sign and notarize:

   ```sh
   scripts/release.sh
   ```

   It needs a "Developer ID Application" identity in the keychain and notarytool credentials under the keychain profile `grasslands-notary` (one-time setup, see the script's header). It prints the zip path and its sha256.

3. Tag and publish:

   ```sh
   git tag v0.1.0 && git push origin v0.1.0
   gh release create v0.1.0 target/release-zip/grasslands-0.1.0-arm64.zip --title "grasslands 0.1.0" --prerelease
   ```

   The asset name must match the cask's `url`: `grasslands-#{version}-arm64.zip`. Livecheck uses `:github_releases`, which includes prereleases.

4. Set `version` and `sha256` in the tap's cask, push it, and check:

   ```sh
   brew style --cask lklyne/tap/grasslands
   brew audit --cask --online lklyne/tap/grasslands
   brew install --cask lklyne/tap/grasslands
   ```

The release builds on Apple Silicon only, so the cask has `depends_on arch: :arm64`. An Intel or universal build needs `cargo build --target x86_64-apple-darwin` plus `lipo`, and libghostty would have to build for that target too. That's untested.

## Third-party notices

`scripts/bundle.sh` writes `Contents/Resources/THIRD-PARTY-NOTICES.txt`: every crate's license via `cargo-about` (`packaging/about.toml`, `packaging/about.hbs`), plus Ghostty's license, gpui-libghostty's MIT notice (its crate ships without one) and cuelume's (ported into `crates/cues`) from `packaging/notices/`. A new dependency under a license not in `about.toml`'s `accepted` list fails the build until it's reviewed and added.

## herdr

The cask doesn't depend on the herdr formula. grasslands uses the herdr on the user's PATH, the same one their terminal runs, and a `depends_on` would put a second, Homebrew herdr next to one from herdr's own installer or mise. On launch the app runs `herdr status --json` and, when herdr is missing, older than 0.9.5, or running an older server, shows a setup screen that installs, updates or restarts it. See `docs/decisions/0018-herdr-setup.md`.
