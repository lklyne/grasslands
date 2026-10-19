//! The dock icon, badge and alert sounds. GPUI has no dock, so this calls
//! AppKit; the sounds are cues, with AppKit's system sounds as a fallback.

use std::path::Path;

use crate::herdr::AgentStatus;
use crate::sounds;

/// Gives an unbundled run (`cargo run`, `scripts/relaunch-main`) the app icon
/// in the dock. A bundle's own icon is left alone: macOS draws its Assets.car
/// as glass, which this flat copy would replace.
pub fn set_icon() {
    #[cfg(target_os = "macos")]
    {
        use objc2::{AllocAnyThread as _, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        static ICON: &[u8] = include_bytes!("../../assets/AppIcon.compiled/AppIcon.icns");

        let bundled = std::env::current_exe()
            .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if bundled {
            return;
        }
        let data = NSData::with_bytes(ICON);
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            // SAFETY: on the main thread, with an image AppKit decoded itself.
            unsafe {
                NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image));
            }
        }
    }
}

/// Shows `count` on the dock icon, or clears it at zero.
pub fn set_badge(count: usize) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;

        // GPUI runs entity callbacks on the main thread; bail rather than
        // touch AppKit from anywhere else.
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = count;
}

/// Plays the sound for an agent that just reached `status`: `Attention` when
/// it's blocked, `Ready` when it's done (strong while the app is in the
/// background, since it finished while you were away). herdr's custom mp3
/// (`[ui.sound] path`, `done_path`, `request_path`) plays instead when set
/// and readable. Sounds off in Settings → Sounds silences both.
pub fn play_for(status: AgentStatus, background: bool, custom: Option<&Path>) {
    let cue = match status {
        AgentStatus::Blocked => cues::Cue::Attention,
        AgentStatus::Done => cues::Cue::Ready,
        _ => return,
    };
    if let Some(path) = custom
        && sounds::enabled()
        && play_file(path)
    {
        return;
    }
    let options = if background && cue == cues::Cue::Ready {
        sounds::strong()
    } else {
        cues::PlayOptions::default()
    };
    match sounds::cues() {
        Some(_) => sounds::play(cue, options),
        None if sounds::enabled() => play_system(status),
        None => {}
    }
}

/// Plays an audio file (herdr's custom mp3s) through AppKit. Returns
/// whether it started; a missing or unplayable file falls back to the cue,
/// as herdr falls back to its default sound. Silent in tests and script
/// runs, which pretend it played.
fn play_file(path: &Path) -> bool {
    if cfg!(test) || std::env::var_os("RT_SCRIPT").is_some() {
        return path.is_file();
    }
    play_file_now(path)
}

#[cfg(target_os = "macos")]
fn play_file_now(path: &Path) -> bool {
    {
        use std::cell::RefCell;

        use objc2::rc::Retained;
        use objc2::{AllocAnyThread as _, MainThreadMarker};
        use objc2_app_kit::NSSound;
        use objc2_foundation::NSString;

        thread_local! {
            /// The playing file, kept alive until the next one replaces it.
            static PLAYING: RefCell<Option<Retained<NSSound>>> = const { RefCell::new(None) };
        }

        if MainThreadMarker::new().is_none() || !path.is_file() {
            return false;
        }
        let file = NSString::from_str(&path.to_string_lossy());
        let Some(sound) =
            NSSound::initWithContentsOfFile_byReference(NSSound::alloc(), &file, true)
        else {
            tracing::warn!(
                "herdr custom sound {}: AppKit can't play it",
                path.display()
            );
            return false;
        };
        let started = sound.play();
        PLAYING.with(|playing| *playing.borrow_mut() = Some(sound));
        started
    }
}

#[cfg(not(target_os = "macos"))]
fn play_file_now(_: &Path) -> bool {
    false
}

/// The system sound for `status`, for when cues couldn't start.
fn play_system(status: AgentStatus) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSSound;
        use objc2_foundation::NSString;

        let name = match status {
            AgentStatus::Blocked => "Funk",
            AgentStatus::Done => "Glass",
            _ => return,
        };
        if let Some(sound) = NSSound::soundNamed(&NSString::from_str(name)) {
            sound.play();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = status;
}
