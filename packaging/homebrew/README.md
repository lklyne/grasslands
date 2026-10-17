# Homebrew cask

`grasslands.rb` is a draft cask for a `lklyne/homebrew-tap` repo, so people can run `brew install --cask lklyne/tap/grasslands`. Its version and sha256 are placeholders. None of the steps below have been done yet.

## 1. Create the tap

Make a public repo named exactly `lklyne/homebrew-tap`. Homebrew maps `lklyne/tap` to it. Copy the cask to `Casks/grasslands.rb` there.

## 2. Pick a version

`crates/app/Cargo.toml` says `0.0.0`, and `scripts/bundle.sh` writes that into `Info.plist`. Bump it before the first release, for example to `0.1.0`, and tag the commit `v0.1.0`.

## 3. Build and cut a release

```sh
scripts/bundle.sh
ditto -c -k --keepParent target/bundle/grasslands.app grasslands-0.1.0-arm64.zip
shasum -a 256 grasslands-0.1.0-arm64.zip
gh release create v0.1.0 grasslands-0.1.0-arm64.zip --title "grasslands 0.1.0" --prerelease
```

Use `ditto`, not `zip`. It keeps the bundle's symlinks, permissions and extended attributes. The asset name must match the cask's `url`: `grasslands-#{version}-arm64.zip`.

Then set `version` and `sha256` in the tap's cask, remove the PLACEHOLDER comments, and check it:

```sh
brew style --cask lklyne/tap/grasslands
brew audit --cask --online lklyne/tap/grasslands
brew install --cask lklyne/tap/grasslands
```

The release builds on Apple Silicon only, so the cask has `depends_on arch: :arm64`. An Intel or universal build needs `cargo build --target x86_64-apple-darwin` plus `lipo`, and libghostty would have to build for that target too. That's untested.

## 4. Signing

The bundle is unsigned, apart from the ad-hoc signature the linker puts on arm64 binaries. Homebrew downloads it with the `com.apple.quarantine` attribute, and Gatekeeper then refuses to open it. On macOS 15 and later, right-click > Open no longer gets around that. The user has to go to System Settings > Privacy & Security > Open Anyway. There are three options:

1. **Sign and notarize.** This needs an Apple Developer account ($99 a year) and a Developer ID Application certificate. Sign with the hardened runtime (`codesign --options runtime --timestamp`), then run `xcrun notarytool submit --wait` and `xcrun stapler staple` on the app before zipping it. After that, delete the cask's `postflight` block. This is the only option that gives a clean first launch. Homebrew's own cask repo will soon require it, but a personal tap doesn't.
2. **Strip quarantine in the cask.** This is what the draft does now. Its `postflight` runs `xattr -dr com.apple.quarantine` on the installed app. It works, and everyone who installs is trusting the release zip without Apple's check. That's a reasonable call for an alpha in a personal tap. Homebrew has deprecated the user-side `--no-quarantine` flag, so a cask that relies on users to pass it won't keep working.
3. **Document it.** Drop the `postflight` block and tell users to run `xattr -dr com.apple.quarantine /Applications/grasslands.app` or use Open Anyway. That's the most friction of the three.

Before notarizing, there's one thing to fix. The bundle's `CFBundleExecutable` is a shell script (`Contents/MacOS/grasslands`). It adds Homebrew and `~/.local/bin` to `PATH`, then execs `grasslands-bin`. Notarization wants the main executable to be a signed Mach-O with the hardened runtime. So move the `PATH` handling into the Rust binary, by looking up `herdr` in those directories when `HERDR_BIN` is unset, and make the binary the bundle's executable.

## 5. Third-party notices

The app statically links code under other licenses, and a binary release has to carry their notices. Nothing in the bundle does that yet. At minimum, ship:

- Ghostty's MIT license (`vendor/gpui-libghostty/vendor/ghostty/LICENSE`).
- gpui-libghostty's MIT notice. The crates.io package has no LICENSE file, so it's missing from `vendor/gpui-libghostty`. Get it from the upstream repo.
- gpui-component / GPUI Kit and Zed's GPUI, Apache-2.0, including any NOTICE files.
- cuelume's MIT notice, for `crates/cues`.

Tools like `cargo about` or `cargo-bundle-licenses` can generate the full list from `Cargo.lock`. Put the output in `Contents/Resources/` from `scripts/bundle.sh`.

## 6. herdr

`brew info herdr` shows herdr 0.9.5 as a homebrew-core formula, so the cask has `depends_on formula: "herdr"`. That can't pin a version. If herdr ships a protocol change, users get the new herdr on `brew upgrade` before grasslands catches up. The caveat says 0.9.5 is the version that works. As far as I can tell, herdr-client doesn't check the server's `protocol` field yet. Checking it at connect time and showing a clear error would make that failure obvious.
