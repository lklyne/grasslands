//! Updates, through Sparkle. Ported from Specular's updater
//! (github.com/lklyne/specular, `native/crates/specular-shell/src/updates.rs`).
//!
//! The app bundle carries `Sparkle.framework` and names its feed and its
//! public key in `Info.plist` (`SUFeedURL`, `SUPublicEDKey`; see
//! `scripts/bundle.sh`). Sparkle checks the feed on its own schedule, shows
//! what it found, downloads the new app, verifies it against the key and
//! swaps it in. See docs/decisions/0024-updates.md.
//!
//! The framework is loaded at run time and never linked, so a build that is
//! not the bundle (`cargo run`, `scripts/dev-run`) has no updater and says
//! so only when asked to check.

use std::path::{Path, PathBuf};

use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use crate::actions::CheckForUpdates;
use crate::sounds;

/// Where the framework is, from the executable in `Contents/MacOS`.
const FRAMEWORK: &str = "../Frameworks/Sparkle.framework";

/// The running version, as `Info.plist` and the feed spell it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn init(cx: &mut App) {
    cx.on_action(|_: &CheckForUpdates, cx| check(cx));
}

/// The embedded framework for the executable at `exe`, if it is there.
fn framework_for(exe: &Path) -> Option<PathBuf> {
    let path = exe.parent()?.join(FRAMEWORK);
    path.is_dir().then_some(path)
}

fn framework() -> Option<PathBuf> {
    framework_for(&std::env::current_exe().ok()?)
}

/// Starts the updater, which then checks the feed on its own schedule (when
/// automatic checks are on). Does nothing in a build with no updater. Call
/// once the main window is open, so Sparkle's first prompt has an app to
/// sit over.
pub fn start() {
    if sparkle::started().is_none() {
        tracing::info!("updates: no updater in this build");
    }
}

/// Whether this build has an updater: it runs from a bundle that carries
/// Sparkle, and the framework loaded.
pub fn available() -> bool {
    sparkle::started().is_some()
}

/// grasslands > Check for Updates…: Sparkle checks now and shows what it
/// found in its own window. With no updater, a toast says so.
pub fn check(cx: &mut App) {
    if sparkle::check() {
        sounds::play(cues::Cue::Open, sounds::subtle());
        return;
    }
    tracing::warn!("updates: this build has no updater to check with");
    sounds::play(cues::Cue::Error, sounds::subtle());
    let message = format!(
        "This build ({VERSION}) has no updater. The grasslands app from GitHub or Homebrew \
         updates itself."
    );
    toast(
        Notification::warning(message).title("Check for Updates"),
        cx,
    );
}

/// Whether Sparkle checks on its own (`SUEnableAutomaticChecks`, then the
/// user's choice, kept in the app's defaults). `None` with no updater.
pub fn automatic_checks() -> Option<bool> {
    sparkle::automatic_checks()
}

/// Turns Sparkle's automatic checks on or off. Sparkle saves the choice.
pub fn set_automatic_checks(on: bool) {
    sparkle::set_automatic_checks(on);
}

/// Shows `note` in the main window's chrome (its overlay, above the
/// terminals). The action may come from any window, Settings included.
fn toast(note: Notification, cx: &mut App) {
    cx.defer(move |cx| {
        let chrome = cx.windows().into_iter().find_map(|window| {
            window
                .update(cx, |_, window, cx| crate::chrome::get(window, cx))
                .ok()
                .flatten()
        });
        if let Some(chrome) = chrome {
            chrome.update(cx, |chrome, cx| {
                chrome.floating(cx, move |window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    window.push_notification(note, cx);
                });
            });
        }
    });
}

#[cfg(target_os = "macos")]
#[expect(
    clippy::multiple_unsafe_ops_per_block,
    reason = "AppKit calls come in runs on the same live objects; one SAFETY note covers a run"
)]
mod sparkle {
    use std::cell::OnceCell;

    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    type Id = *mut AnyObject;

    thread_local! {
        /// Sparkle's `SPUStandardUpdaterController`, kept for the process's
        /// life, or nothing in a build with no updater. AppKit's main thread
        /// is the only one that reaches it.
        static CONTROLLER: OnceCell<Option<Id>> = const { OnceCell::new() };
    }

    fn controller() -> Option<Id> {
        let path = super::framework()?;
        let path = NSString::from_str(&path.to_string_lossy());
        let nil: Id = std::ptr::null_mut();
        // SAFETY: `path` is a live `NSString`, and `bundleWithPath:` and
        // `load` take and return what `NSBundle` declares. The controller
        // class is looked up only after its framework loaded, and its
        // initializer takes a `BOOL` and two delegates, both nil here. The
        // controller is never released, as it runs the updater for as long
        // as the app does.
        unsafe {
            let bundle: Id = msg_send![class!(NSBundle), bundleWithPath: &*path];
            if bundle.is_null() {
                return None;
            }
            let loaded: Bool = msg_send![bundle, load];
            if !loaded.as_bool() {
                tracing::warn!("updates: Sparkle.framework did not load");
                return None;
            }
            let class = AnyClass::get(c"SPUStandardUpdaterController")?;
            let controller: Id = msg_send![class, alloc];
            let controller: Id = msg_send![
                controller,
                initWithStartingUpdater: Bool::YES,
                updaterDelegate: nil,
                userDriverDelegate: nil
            ];
            (!controller.is_null()).then_some(controller)
        }
    }

    pub(super) fn started() -> Option<Id> {
        CONTROLLER.with(|cell| *cell.get_or_init(controller))
    }

    /// The controller's `SPUUpdater`.
    fn updater() -> Option<Id> {
        let controller = started()?;
        // SAFETY: `controller` is the live controller made above, and
        // `updater` returns its updater, which lives as long as it does.
        let updater: Id = unsafe { msg_send![controller, updater] };
        (!updater.is_null()).then_some(updater)
    }

    pub(super) fn check() -> bool {
        let Some(controller) = started() else {
            return false;
        };
        let nil: Id = std::ptr::null_mut();
        // SAFETY: `controller` is the live controller made above, and
        // `checkForUpdates:` takes a sender, which may be nil.
        let _: () = unsafe { msg_send![controller, checkForUpdates: nil] };
        true
    }

    pub(super) fn automatic_checks() -> Option<bool> {
        let updater = updater()?;
        // SAFETY: `updater` is live; the property is a `BOOL`.
        let on: Bool = unsafe { msg_send![updater, automaticallyChecksForUpdates] };
        Some(on.as_bool())
    }

    pub(super) fn set_automatic_checks(on: bool) {
        let Some(updater) = updater() else {
            return;
        };
        // SAFETY: `updater` is live; the setter takes a `BOOL`.
        let _: () = unsafe { msg_send![updater, setAutomaticallyChecksForUpdates: Bool::new(on)] };
    }
}

#[cfg(not(target_os = "macos"))]
mod sparkle {
    pub(super) fn started() -> Option<()> {
        None
    }

    pub(super) fn check() -> bool {
        false
    }

    pub(super) fn automatic_checks() -> Option<bool> {
        None
    }

    pub(super) fn set_automatic_checks(_: bool) {}
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::framework_for;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grasslands-updates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_bundle_with_sparkle_has_an_updater() {
        let app = scratch("bundle").join("grasslands.app/Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Frameworks/Sparkle.framework")).unwrap();
        let found = framework_for(&app.join("MacOS/grasslands"));
        assert!(found.is_some_and(|path| path.ends_with("Sparkle.framework")));
    }

    #[test]
    fn a_bare_binary_has_no_updater() {
        let dir = scratch("bare").join("target/debug");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(framework_for(&dir.join("grasslands")), None);
    }

    #[test]
    fn the_cargo_run_binary_has_no_updater() {
        assert_eq!(framework_for(Path::new("/")), None);
    }

    /// The Info.plist values `scripts/bundle.sh` writes: the feed on the
    /// latest GitHub release, and a public key that is an EdDSA (32-byte)
    /// key in base64 once it is filled in.
    #[test]
    fn bundle_script_names_the_feed_and_a_well_formed_key() {
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/bundle.sh"),
        )
        .unwrap();
        let value = |name: &str| {
            script
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=\"")))
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        };
        assert_eq!(
            value("FEED_URL").as_deref(),
            Some("https://github.com/lklyne/grasslands/releases/latest/download/appcast.xml")
        );
        let key = value("UPDATE_PUBLIC_KEY").unwrap_or_default();
        let base64 = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
        assert!(
            key.is_empty()
                || (key.len() == 44 && key.ends_with('=') && key[..43].chars().all(base64)),
            "UPDATE_PUBLIC_KEY is not a base64 EdDSA public key: {key}"
        );
    }
}
