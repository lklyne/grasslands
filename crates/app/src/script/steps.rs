//! Debug builds only: a `;`-separated list of steps run after startup, so a
//! flow can be driven live (against the dev session) without synthetic OS
//! input. One language for the whole app, read from `RT_STEPS`;
//! `RT_CHROME_SCRIPT` and `RT_SIDEBAR_SCRIPT` are older names for it that
//! keep their own pacing and log prefix. Each list runs on its own.
//!
//! Anywhere:
//! - `wait <ms>`
//! - `key <keystroke>`: one key press (`down`, `enter`, `escape`, `cmd-a`,
//!   …) to the window that owns the keyboard: the overlay while a menu, the
//!   palette or a dialog is open there, else the main window
//!   (`overlay::dispatch_keystroke`)
//! - `keys <text>`: type `text` one key press at a time, the same way
//! - `scroll <x> <y> <dy>`: a wheel event at window point (x, y), straight
//!   into GPUI (AppKit's native views and the overlay never see it)
//! - `hover <x> <y>`: a mouse move to window point (x, y), the same way
//! - `overlay`: open the overlay window, as the first toast or menu would
//! - `check-drag <label>`: print whether a drag is active and whether the
//!   overlay's last frame was drawn during one (and so shows its preview)
//! - `log <label>`: print what the chrome has open, herdr's workspaces, tabs,
//!   panes and agents, and the sidebar's rows
//!
//! The chrome (`chrome/script.rs`): `menu`, `run`, `open-worktree`,
//! `remove-worktree`, `palette`, `query`, `type`.
//!
//! The sidebar (`sidebar/workspaces/script.rs`): `focus`, `agent`, `fold`,
//! `sort`, `new-workspace`, `new-worktree`, `worktree`, `new-worktree-in`,
//! `repo-worktree`, `rename`, `close`, `move`, and:
//! - `drag <workspace> <target>`: drag a row with GPUI mouse events and
//!   release it over `target`: a workspace's row, `off` (the panel below the
//!   rows), `outside` (the window) or `cancel` (the app stops the drag before
//!   the release). The overlay is drawn at each step, as a toast or tooltip
//!   would; after the release nothing is drawn on the script's behalf.
//!
//! e.g. `RT_STEPS='menu pane w1:p1; wait 1500; key escape; log after'`.

use std::time::Duration;

use gpui_kit::*;

use crate::overlay;

/// Where a step list comes from, and how it is paced.
struct Source {
    var: &'static str,
    /// Prefix of every line it prints.
    tag: &'static str,
    /// Wait before the first step (for the first snapshot).
    lead: Duration,
    /// Pause after each step, so it renders (and can be screenshotted).
    pause: Duration,
}

const SOURCES: &[Source] = &[
    Source {
        var: "RT_STEPS",
        tag: "steps",
        lead: Duration::from_secs(2),
        pause: Duration::from_millis(400),
    },
    Source {
        var: "RT_CHROME_SCRIPT",
        tag: "chrome-script",
        lead: Duration::from_secs(2),
        pause: Duration::from_millis(400),
    },
    Source {
        var: "RT_SIDEBAR_SCRIPT",
        tag: "sidebar-script",
        lead: Duration::ZERO,
        pause: Duration::ZERO,
    },
];

/// Starts each step list the environment sets, in the main window `window`.
pub fn start(window: AnyWindowHandle, cx: &mut App) {
    for source in SOURCES {
        let Ok(list) = std::env::var(source.var) else {
            continue;
        };
        let steps = parse(&list);
        window
            .update(cx, |_, window, cx| {
                window
                    .spawn(cx, async move |cx| run(source, steps, cx).await)
                    .detach();
            })
            .ok();
    }
}

/// The steps of `list`, trimmed, empty ones dropped.
fn parse(list: &str) -> Vec<String> {
    list.split(';')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn run(source: &'static Source, steps: Vec<String>, cx: &mut AsyncWindowContext) {
    let executor = cx.background_executor().clone();
    executor.timer(source.lead).await;
    let tag = source.tag;
    for step in steps {
        eprintln!("[{tag}] {step}");
        let (verb, arg) = step.split_once(' ').unwrap_or((step.as_str(), ""));
        let arg = arg.trim();
        // Every update names the main window: `update_in` on an entity would
        // use the window it was last used in, the overlay once a palette or
        // prompt has run there (decision 0008).
        let done = match verb {
            "wait" => {
                let ms = arg.parse().unwrap_or(500);
                executor.timer(Duration::from_millis(ms)).await;
                continue;
            }
            "drag" => drag(tag, arg, cx).await,
            "key" | "keys" => {
                let keys = if verb == "key" {
                    Keystroke::parse(arg).map(|key| vec![key])
                } else {
                    Ok(overlay::keystrokes(arg))
                };
                match keys {
                    // Keys come from the window, like real input: the
                    // overlay when something modal is open there.
                    Ok(keys) => cx.update(|window, cx| {
                        for key in keys {
                            overlay::dispatch_keystroke(key, window, cx);
                        }
                    }),
                    Err(error) => {
                        eprintln!("[{tag}] {step}: {error}");
                        Ok(())
                    }
                }
            }
            "scroll" | "hover" => {
                let event = if verb == "scroll" {
                    wheel_event(arg)
                } else {
                    move_event(arg)
                };
                match event {
                    Some(event) => cx.update(|window, cx| {
                        let propagated = window.dispatch_event(event, cx).propagate;
                        eprintln!("[{tag}] {step}: propagated={propagated}");
                    }),
                    None => {
                        eprintln!("[{tag}] {step}: bad coordinates");
                        Ok(())
                    }
                }
            }
            _ => cx.update(|window, cx| run_step(tag, verb, arg, &step, window, cx)),
        };
        if done.is_err() {
            // The window is gone.
            return;
        }
        executor.timer(source.pause).await;
    }
    eprintln!("[{tag}] end");
}

/// A step that runs in one update of the main window.
fn run_step(tag: &str, verb: &str, arg: &str, step: &str, window: &mut Window, cx: &mut App) {
    match verb {
        "overlay" => {
            if let Err(error) = overlay::get_or_open(window, cx) {
                eprintln!("[{tag}] overlay: {error:#}");
            }
        }
        "check-drag" => {
            let drew = overlay::get(window, cx).map(|overlay| overlay.read(cx).drew_drag(cx));
            eprintln!(
                "[{tag}] check {arg}: drag_active={} overlay_drew_drag={drew:?}",
                cx.has_active_drag(),
            );
        }
        "log" => {
            crate::chrome::script::log(tag, arg, window, cx);
            crate::sidebar::workspaces::script::log(tag, arg, cx);
        }
        _ => {
            let result = crate::chrome::script::run_step(step, window, cx)
                .or_else(|| crate::sidebar::workspaces::script::run_step(step, window, cx));
            match result {
                Some(Ok(())) => {}
                Some(Err(error)) => eprintln!("[{tag}] {step}: {error}"),
                None => eprintln!("[{tag}] unknown step {verb:?}"),
            }
        }
    }
}

fn numbers(args: &str) -> Option<Vec<f32>> {
    args.split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()
}

fn move_event(args: &str) -> Option<PlatformInput> {
    let [x, y] = numbers(args)?[..] else {
        return None;
    };
    Some(PlatformInput::MouseMove(MouseMoveEvent {
        position: point(px(x), px(y)),
        pressed_button: None,
        modifiers: Modifiers::none(),
    }))
}

fn wheel_event(args: &str) -> Option<PlatformInput> {
    let [x, y, dy] = numbers(args)?[..] else {
        return None;
    };
    Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
        position: point(px(x), px(y)),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    }))
}

/// The `drag` step; see the module docs. Errs only when the window is gone.
async fn drag(tag: &str, arg: &str, cx: &mut AsyncWindowContext) -> anyhow::Result<()> {
    let (dragged, target) = arg.split_once(' ').unwrap_or((arg, ""));
    let (dragged, target) = (dragged.trim(), target.trim());
    let rows = cx.update(|_, cx| crate::sidebar::workspaces::script::drawn_rows(cx))?;
    let Some(from) = rows.get(dragged).map(|b| b.center()) else {
        eprintln!("[{tag}] drag: no row for {dragged}");
        return Ok(());
    };
    let lowest = rows.values().map(|b| b.bottom()).fold(from.y, Pixels::max);
    let to = match target {
        "off" => point(from.x, lowest + px(60.)),
        "outside" => point(px(-200.), px(-200.)),
        "cancel" => point(from.x, lowest + px(30.)),
        id => match rows.get(id) {
            Some(bounds) => bounds.center(),
            None => {
                eprintln!("[{tag}] drag: no row for {id}");
                return Ok(());
            }
        },
    };
    let executor = cx.background_executor().clone();
    let pause = || executor.timer(Duration::from_millis(40));
    cx.update(|window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: from,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
    })?;
    pause().await;
    const STEPS: usize = 10;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        let position = from + (to - from) * t;
        let handle = cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            overlay::get(window, cx).map(|overlay| overlay.read(cx).window_handle())
        })?;
        if let Some(handle) = handle {
            cx.update_window(handle, |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .ok();
        }
        pause().await;
    }
    cx.update(|window, cx| {
        if target == "cancel" {
            cx.stop_active_drag(window);
        }
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })?;
    // Let frames come on their own: one the app asks for, none forced here.
    executor.timer(Duration::from_millis(400)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{move_event, parse, wheel_event};

    #[test]
    fn steps_are_split_on_semicolons_and_trimmed() {
        assert_eq!(
            parse(" menu pane w1:p1 ;; wait 1500;key escape ; "),
            ["menu pane w1:p1", "wait 1500", "key escape"]
        );
    }

    #[test]
    fn pointer_steps_need_every_coordinate() {
        assert!(move_event("10 20").is_some());
        assert!(move_event("10").is_none());
        assert!(wheel_event("10 20 -30").is_some());
        assert!(wheel_event("10 20 x").is_none());
    }
}
