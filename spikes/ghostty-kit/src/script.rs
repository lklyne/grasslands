//! `--script`: drives the spike through each claim and screenshots it into
//! `shots/`. Captures are of this app's own window only.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use crate::{Spike, native};

fn shots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shots")
}

async fn wait(cx: &mut AsyncApp, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

/// Captures the main window alone (`screencapture -l`), which leaves out the
/// overlay child window.
fn shot_window(number: isize, name: &str) {
    capture(&["-o", "-l", &number.to_string()], name);
}

/// Captures the main window's on-screen rectangle, so child windows above it
/// (the overlay) are in the picture. The app is frontmost when this runs.
fn shot_region(window: &Window, name: &str) {
    let Some(ns_window) = native::ns_window(window) else {
        return;
    };
    let frame = ns_window.frame();
    let Some(screen) = ns_window.screen() else {
        return;
    };
    let screen_height = screen.frame().size.height;
    let top = screen_height - frame.origin.y - frame.size.height;
    let region = format!(
        "{},{},{},{}",
        frame.origin.x, top, frame.size.width, frame.size.height
    );
    capture(&["-R", &region], name);
}

fn capture(args: &[&str], name: &str) {
    let path = shots_dir().join(format!("{name}.png"));
    let _ = std::fs::create_dir_all(shots_dir());
    let status = std::process::Command::new("screencapture")
        .arg("-x")
        .args(args)
        .arg(&path)
        .status();
    eprintln!("shot {name}: {status:?} -> {}", path.display());
}

/// Runs `tools/post-events` (built to target/) against this process.
fn post_events(args: &[&str]) -> Option<std::process::Child> {
    let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/post-events");
    std::process::Command::new(tool)
        .arg(std::process::id().to_string())
        .args(args)
        .spawn()
        .inspect_err(|error| eprintln!("post-events: {error}"))
        .ok()
}

async fn finish(cx: &mut AsyncApp, child: Option<std::process::Child>) {
    let Some(mut child) = child else { return };
    while matches!(child.try_wait(), Ok(None)) {
        wait(cx, 50).await;
    }
}

/// The divider's position in global top-left screen coordinates.
fn divider_on_screen(window: &Window, left_width: f64) -> Option<(f64, f64)> {
    let ns_window = native::ns_window(window)?;
    let content = ns_window.contentRectForFrameRect(ns_window.frame());
    let screen_height = ns_window.screen()?.frame().size.height;
    let content_top = screen_height - content.origin.y - content.size.height;
    Some((content.origin.x + left_width, content_top + 300.0))
}

fn type_text(window: &mut Window, cx: &mut App, text: &str) {
    for ch in text.chars() {
        let (key, key_char) = match ch {
            ' ' => ("space".to_owned(), Some(" ".to_owned())),
            '\n' => ("enter".to_owned(), None),
            ch => (ch.to_string(), Some(ch.to_string())),
        };
        window.dispatch_keystroke(
            Keystroke {
                modifiers: Modifiers::default(),
                key,
                key_char,
            },
            cx,
        );
    }
}

/// Logs, once per GPUI frame for `frames` frames, the divider position and
/// every Ghostty view's frame, so view placement can be checked against the
/// layout without a screenshot.
fn probe(window: &mut Window, spike: Entity<Spike>, label: &'static str, frames: u32) {
    if frames == 0 {
        return;
    }
    window.on_next_frame(move |window, cx| {
        let left = spike.read(cx).left_width(cx);
        let views: Vec<String> = native::child_view_frames(window)
            .into_iter()
            .map(|(x, y, w, h, hidden)| {
                format!(
                    "[x={x:.0} y={y:.0} w={w:.0} h={h:.0}{}]",
                    if hidden { " hidden" } else { "" }
                )
            })
            .collect();
        eprintln!("probe {label}: divider={left:.0} views={}", views.join(" "));
        probe(window, spike, label, frames - 1);
    });
    window.refresh();
}

pub async fn run(window: AnyWindowHandle, spike: Entity<Spike>, cx: &mut AsyncApp) {
    wait(cx, 2500).await;
    let Ok(Some(number)) = window.update(cx, |_, window, _| native::window_number(window)) else {
        eprintln!("script: no window number");
        return;
    };
    eprintln!("script: main window number {number}");
    shot_window(number, "1-initial");

    // Typing through GPUI's key dispatch into the focused (left) terminal.
    let _ = window.update(cx, |_, window, cx| {
        type_text(window, cx, "echo typed-into-left-$((6*7))\n");
    });
    wait(cx, 1200).await;
    shot_window(number, "2-typed-left");

    // Real NSEvents, posted to this process only.
    finish(cx, post_events(&["type", "echo real keys from cgevents\n"])).await;
    wait(cx, 1000).await;
    shot_window(number, "2b-real-keys");
    eprintln!("left pane after real keys:\n{}", crate::herdr::read_pane(0));

    // A real divider drag: shoot mid-motion and while held at the end.
    let left = spike.update(cx, |spike, cx| spike.left_width(cx));
    let Ok(Some((x, y))) = window.update(cx, |_, window, _| divider_on_screen(window, left)) else {
        return;
    };
    eprintln!("script: dragging divider from {x},{y}");
    let probed = spike.clone();
    let _ = window.update(cx, |_, window, _| probe(window, probed, "drag", 200));
    let drag = post_events(&[
        "drag",
        &x.to_string(),
        &y.to_string(),
        &(x - 320.0).to_string(),
        &y.to_string(),
        "90",
        "1500",
    ]);
    wait(cx, 900).await;
    shot_window(number, "2c-mid-drag");
    wait(cx, 1300).await;
    shot_window(number, "2d-drag-held");
    finish(cx, drag).await;
    wait(cx, 500).await;
    shot_window(number, "2e-drag-released");
    let after = spike.update(cx, |spike, cx| spike.left_width(cx));
    eprintln!("script: divider {left:.0} -> {after:.0} after a real drag of -320");
    let probed = spike.clone();
    let _ = window.update(cx, |_, window, _| probe(window, probed, "released", 1));

    for (index, width) in [300.0, 860.0, 600.0].into_iter().enumerate() {
        let _ = window.update(cx, |_, window, cx| {
            spike.update(cx, |spike, cx| spike.resize_divider(px(width), window, cx));
        });
        wait(cx, 700).await;
        shot_window(number, &format!("3-divider-{index}-{width}"));
    }

    let _ = window.update(cx, |_, window, _| window.resize(size(px(900.), px(560.))));
    wait(cx, 900).await;
    shot_window(number, "4-window-small");
    let probed = spike.clone();
    let _ = window.update(cx, |_, window, _| probe(window, probed, "window-small", 1));

    spike.update(cx, |spike, cx| spike.set_right_visible(false, cx));
    wait(cx, 700).await;
    shot_window(number, "5-right-hidden");
    let _ = window.update(cx, |_, window, _| window.resize(size(px(1300.), px(760.))));
    wait(cx, 900).await;
    shot_window(number, "6-right-hidden-after-resize");
    let probed = spike.clone();
    let _ = window.update(cx, |_, window, _| {
        probe(window, probed, "hidden-after-resize", 1)
    });
    spike.update(cx, |spike, cx| spike.set_right_visible(true, cx));
    wait(cx, 900).await;
    shot_window(number, "7-right-shown");
    let probed = spike.clone();
    let _ = window.update(cx, |_, window, _| probe(window, probed, "shown", 1));

    spike.update(cx, |spike, cx| spike.swap_config(cx));
    wait(cx, 900).await;
    shot_window(number, "8-right-config-swapped");
    spike.update(cx, |spike, cx| spike.swap_config(cx));

    let _ = window.update(cx, |_, window, cx| {
        spike.update(cx, |spike, cx| spike.toggle_popover(window, cx));
    });
    wait(cx, 1000).await;
    let _ = window.update(cx, |_, window, _| shot_region(window, "9-overlay-popover"));
    let _ = window.update(cx, |_, window, _| window.resize(size(px(1000.), px(640.))));
    wait(cx, 900).await;
    let _ = window.update(cx, |_, window, _| {
        shot_region(window, "10-overlay-after-resize")
    });

    eprintln!("script: done");
    if std::env::args().any(|arg| arg == "--quit") {
        cx.update(|cx| cx.quit());
    }
}
