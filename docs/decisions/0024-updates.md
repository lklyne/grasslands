# 0024: updates through Sparkle

grasslands updates itself with [Sparkle](https://sparkle-project.org) 2.10.0,
the way Specular does (github.com/lklyne/specular, branch `rust-native`:
`native/crates/specular-shell/src/updates.rs`, `native/scripts/fetch-sparkle.sh`
and the signing and feed steps of `bundle-app.sh`). The Rust side, the fetch
script and the order things are signed in are ported from there.

## How it fits together

- `scripts/fetch-sparkle.sh` downloads Sparkle's distribution, checks it
  against a pinned sha256 and caches it in `~/.local/share/sparkle/<version>`
  (the same folder Specular uses).
- `scripts/bundle.sh` copies `Sparkle.framework` into
  `Contents/Frameworks/` and writes `SUFeedURL`, `SUPublicEDKey`,
  `SUEnableAutomaticChecks` (on) and `SUScheduledCheckInterval` (a day) into
  `Info.plist`. It signs the bundle ad hoc; `scripts/sign-bundle.sh` signs
  inside out (the installer and downloader XPC services, the latter keeping
  its entitlements, then `Autoupdate`, `Updater.app`, the framework, the app).
  Sparkle's docs give that order and advise against `codesign --deep`.
- `scripts/release.sh` signs with the Developer ID and the hardened runtime,
  notarizes, staples, zips (`ditto --sequesterRsrc`), signs the zip with
  `sign_update` and writes `appcast.xml` next to it. Both go on the GitHub
  release.
- `crates/app/src/updates.rs` loads the framework at run time through the
  Objective-C runtime (it is never linked) and keeps one
  `SPUStandardUpdaterController`, started once the main window is up.
  `cargo run` and `scripts/dev-run` builds have no framework next to them, so
  they have no updater; asking them to check shows a toast that says so.
- grasslands > Check for Updates… and the palette's "Check for Updates…" call
  `checkForUpdates:`; Sparkle shows what it found in its own window, with
  release notes, Install and Skip. Settings > Updates shows the version, a
  Check for Updates button and Sparkle's "check automatically" switch
  (`automaticallyChecksForUpdates`, kept by Sparkle in the app's defaults,
  not in `settings.json`).

## Feed and releases

The feed is `https://github.com/lklyne/grasslands/releases/latest/download/appcast.xml`.
It never changes address, because `latest` redirects to the newest release.
GitHub's `latest` skips drafts and prereleases, and every release up to
0.1.1 was a prerelease, so releases are regular from 0.1.2 on. Each
release's `appcast.xml` names only its own zip, and that's enough: Sparkle
only needs the newest version.

0.1.1 and older have no updater. Their users move to 0.1.2 by hand, or with
`brew upgrade --cask grasslands`. Updates through Sparkle start with 0.1.2 → 0.1.3.

## Signing key

Updates are signed with an EdDSA (ed25519) key that belongs to grasslands
alone, not Specular's. Sparkle's `generate_keys --account grasslands` makes
it, keeps the private half in the maintainer's login keychain under the
account `grasslands`, and prints the public half, which goes into
`UPDATE_PUBLIC_KEY` in `scripts/bundle.sh`. The private key is never printed,
copied or committed. CI, if it ever signs releases, gets it as the
`SPARKLE_PRIVATE_KEY` secret (`generate_keys -x` exports it). Keep a backup
of the keychain item: without it, existing installs can't verify the next
update, and moving to a new key means following Sparkle's key rotation
steps, which lean on the Developer ID signature staying the same.

## Privacy

One unauthenticated HTTPS GET for `appcast.xml` from GitHub a day, plus the
download when an update is installed. Sparkle's system profiling
(`SUEnableSystemProfiling`) is left off, so the request carries nothing but
Sparkle's user agent (app name and version). There's no telemetry. Turning
off "Check automatically" stops the daily request.

## Homebrew

The cask says `auto_updates true`. Sparkle replaces
`/Applications/grasslands.app` in place, and `brew upgrade` leaves
auto-updating casks alone unless run with `--greedy`, so the two don't fight.
Homebrew's recorded version lags behind until the next greedy upgrade, which
is harmless. The tap still gets each release's version and sha256, for new
installs.

## Not done

A first plan was notify-only: poll GitHub's releases API, show a toast with
Download or `brew upgrade --cask grasslands`, no installing. Sparkle replaced
it before it shipped. Sparkle's signed feed, its dialog (release notes, skip
this version, remind later) and its in-place install do all of that and more.
Specular already has it working with this toolchain. It costs one embedded
framework (MIT, in the third-party notices) and a signing key to look after.
