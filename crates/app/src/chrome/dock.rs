//! The dock badge and alert sounds. GPUI has neither, so this calls AppKit.

use crate::herdr::AgentStatus;

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

/// Plays the system sound for an agent that just reached `status`.
pub fn play_for(status: AgentStatus) {
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
