//! Debug builds only: the live test harness. Started from `main` once the
//! main window is open, it runs one `RT_SCRIPT` scenario against the dev
//! herdr session (see [`SCENARIOS`] and docs/development.md), and the
//! `RT_STEPS` step list ([`steps`]), printing `ok`/`FAIL` lines to stderr
//! and saving screenshots of this app's own windows to
//! `target/script-shots/`.
//!
//! ```sh
//! RT_SCRIPT=terminals scripts/dev-run cargo run -p app
//! ```
//!
//! Input is dispatched inside the app (GPUI keystrokes, actions and mouse
//! events), never through the OS. Each check reads state back: herdr's
//! snapshot, the text herdr holds for a pane, and the frames of the native
//! Ghostty views compared with the slots the layout drew. If the screen is
//! locked GPUI stops drawing, so the harness forces frames itself;
//! screenshots then fail and are reported as skipped.
//!
//! Scenarios reach the views through their public test surface: the
//! window's [`Workspace`], `chrome::get`, `overlay::get`, the sidebar's
//! panels and [`Hooks`] for the terminal area. A scenario quits the app when
//! done unless `RT_SCRIPT_KEEP=1`, exiting 1 when any check failed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;

mod chrome;
mod drags;
mod herdr_prefs;
mod machine_ui;
mod machines;
mod remote;
mod remote_files;
mod settings;
mod setup;
pub mod steps;
mod terminals;
mod titlebar;

use crate::herdr::{PaneId, SplitTree, TabId, method, protocol};
use crate::overlay;
use crate::state::{AppState, MachineId};
use crate::terminal_area::TerminalArea;
use crate::terminal_area::hooks::{Drawn, Hooks};
use crate::workspace::{self, Workspace};

/// Every `RT_SCRIPT` scenario, and what it needs besides the dev session.
pub const SCENARIOS: &[(&str, &str)] = &[
    ("terminals", ""),
    ("chrome", ""),
    ("titlebar", ""),
    ("settings", ""),
    ("herdr-prefs", "HERDR_CONFIG_PATH at a scratch config.toml"),
    ("drags", ""),
    ("setup", "RT_SETUP_EXPECT, RT_SETUP_FIX"),
    ("machine-ui", ""),
    ("machines", "GRASSLANDS_DEV_MACHINES=dockerbox"),
    ("remote", "GRASSLANDS_DEV_MACHINES=dockerbox"),
    ("remote-files", "GRASSLANDS_DEV_MACHINES=dockerbox"),
];

/// Starts what the environment asks for in the main window `window`: an
/// `RT_SCRIPT` scenario and the `RT_STEPS` steps.
pub fn start(window: AnyWindowHandle, cx: &mut App) {
    if let Some(scenario) = std::env::var("RT_SCRIPT").ok().filter(|s| !s.is_empty()) {
        start_scenario(scenario, window, cx);
    }
    steps::start(window, cx);
}

fn start_scenario(scenario: String, window: AnyWindowHandle, cx: &mut App) {
    if !SCENARIOS.iter().any(|(name, _)| *name == scenario) {
        let names: Vec<&str> = SCENARIOS.iter().map(|(name, _)| *name).collect();
        eprintln!("script: unknown scenario {scenario:?}; one of {names:?}");
        std::process::exit(2);
    }
    // Each run checks only the cues it caused.
    crate::sounds::played::clear();
    // herdr's `ui.prompt_new_tab_name` and `ui.prompt_new_workspace_name`
    // would stop cmd-t and New Workspace at a name dialog, and its default
    // `ui.toast.delivery = "off"` (with a 1 s delay) would hide the agent
    // toasts; the scenarios check what follows. `herdr-prefs` checks the
    // prefs themselves.
    if scenario != "herdr-prefs" {
        use crate::herdr::config::{Edit, ToastDelivery};
        crate::herdr_prefs::override_for_script(Edit::PromptNewTabName(false), cx);
        crate::herdr_prefs::override_for_script(Edit::PromptNewWorkspaceName(false), cx);
        crate::herdr_prefs::override_for_script(Edit::ToastDelivery(ToastDelivery::Herdr), cx);
        crate::herdr_prefs::override_for_script(Edit::ToastDelay(0), cx);
    }
    let name = scenario.clone();
    let started = window.update(cx, |_, window, cx| {
        let scenario = name;
        let area = workspace::get(window, cx)?.read(cx).terminal_area().clone();
        window
            .spawn(cx, async move |cx| {
                let mut run = Run {
                    area,
                    failures: 0,
                    shot: 0,
                };
                let result = match scenario.as_str() {
                    "chrome" => run.chrome(cx).await,
                    "titlebar" => run.titlebar(cx).await,
                    "settings" => run.settings(cx).await,
                    "herdr-prefs" => run.herdr_prefs(cx).await,
                    "drags" => run.drags(cx).await,
                    "setup" => run.setup(cx).await,
                    "machine-ui" => run.machine_ui(cx).await,
                    "machines" => run.machines(cx).await,
                    "remote" => run.remote(cx).await,
                    "remote-files" => run.remote_files(cx).await,
                    _ => run.terminals(cx).await,
                };
                if let Err(error) = result {
                    run.fail(&format!("script stopped: {error:#}"));
                }
                run.finish(cx);
            })
            .detach();
        Some(())
    });
    if !matches!(started, Ok(Some(()))) {
        eprintln!("script: no main window to run {scenario:?} in");
        std::process::exit(2);
    }
}

struct Run {
    area: Entity<TerminalArea>,
    failures: usize,
    shot: usize,
}

type Cx = AsyncWindowContext;

impl Run {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            eprintln!("ok   {what}");
        } else {
            self.fail(what);
        }
    }

    fn fail(&mut self, what: &str) {
        self.failures += 1;
        eprintln!("FAIL {what}");
    }

    /// Reports the tally and quits, unless `RT_SCRIPT_KEEP` is set: exit
    /// status 1 when any check failed, so a run can be scripted.
    fn finish(&self, cx: &mut Cx) {
        eprintln!(
            "script: done, {} failure{}",
            self.failures,
            if self.failures == 1 { "" } else { "s" }
        );
        if std::env::var("RT_SCRIPT_KEEP").is_ok() {
            return;
        }
        if self.failures == 0 {
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }
        // AppKit's terminate always exits 0: run the quit handlers here, as
        // it would, then exit with the failure.
        let app: &mut AsyncApp = cx;
        app.update(App::shutdown);
        std::process::exit(1);
    }

    async fn sleep(cx: &mut Cx, millis: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(millis))
            .await;
    }

    /// Lets GPUI lay out and paint. A locked screen stops the display link,
    /// so draw directly as well, the overlay window included (its hit tests
    /// decide mouse capture).
    async fn settle(cx: &mut Cx) {
        for _ in 0..3 {
            let overlay = cx
                .update(|window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                    overlay::get(window, cx).map(|o| o.read(cx).window_handle())
                })
                .ok()
                .flatten();
            if let Some(overlay) = overlay {
                cx.update_window(overlay, |_, window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                })
                .ok();
            }
            Self::sleep(cx, 60).await;
        }
    }

    /// Polls `test` until it holds or `timeout` passes.
    async fn until(
        &self,
        cx: &mut Cx,
        timeout: Duration,
        mut test: impl FnMut(&Hooks, &Window, &App) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            Self::settle(cx).await;
            let area = self.area.clone();
            let done = cx
                .update(|window, cx| test(&area.read(cx).hooks(), window, cx))
                .unwrap_or(false);
            if done {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            Self::sleep(cx, 100).await;
        }
    }

    fn read<R>(&self, cx: &mut Cx, f: impl FnOnce(&Hooks, &App) -> R) -> anyhow::Result<R> {
        let area = self.area.clone();
        cx.update(|_, cx| f(&area.read(cx).hooks(), cx))
    }

    /// Runs `f` on the app state with the selected machine, the one the
    /// script drives.
    fn on_state<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut AppState, &MachineId, &mut Context<AppState>) -> R,
    ) -> anyhow::Result<R> {
        let area = self.area.clone();
        cx.update(|_, cx| {
            let state = area.read(cx).hooks().state().clone();
            state.update(cx, |state, cx| {
                let machine = state.selected().clone();
                f(state, &machine, cx)
            })
        })
    }

    /// Runs `f` on the window's workspace.
    fn workspace<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> anyhow::Result<R> {
        cx.update(|window, cx| {
            let workspace =
                workspace::get(window, cx).ok_or_else(|| anyhow::anyhow!("no workspace"))?;
            Ok(workspace.update(cx, |ws, cx| f(ws, window, cx)))
        })?
    }

    /// Every screenshot goes through `scripts/capture-windows.swift`: this
    /// app's windows only, whatever else is on screen.
    fn shoot(&mut self, cx: &mut Cx, name: &str) {
        self.shoot_with_overlay(cx, name);
    }

    /// Captures this app's main window composited with its overlay child
    /// window, and nothing else: other apps' windows on top are left out (a
    /// screen-region capture would show whatever is frontmost there).
    fn shoot_with_overlay(&mut self, cx: &mut Cx, name: &str) {
        self.shot += 1;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = root.join("target/script-shots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{:02}-{name}.png", self.shot));
        let numbers = cx
            .update(|window, cx| {
                let main = overlay::ns_window(window)?.windowNumber();
                let overlay = overlay::get(window, cx).and_then(|overlay| {
                    let handle = overlay.read(cx).window_handle();
                    cx.update_window(handle, |_, window, _| {
                        overlay::ns_window(window).map(|w| w.windowNumber())
                    })
                    .ok()
                    .flatten()
                });
                Some((main, overlay))
            })
            .ok()
            .flatten();
        let Some((main, overlay)) = numbers else {
            eprintln!("shot {name} skipped: no window");
            return;
        };
        let mut command = std::process::Command::new("swift");
        command
            .arg(root.join("scripts/capture-windows.swift"))
            .arg(&path)
            .arg(main.to_string());
        if let Some(overlay) = overlay {
            command.arg(overlay.to_string());
        }
        match capture(command, Duration::from_secs(20)) {
            Ok(()) if path.exists() => eprintln!("shot {}", path.display()),
            Ok(()) => eprintln!("shot {name} skipped: no file written"),
            Err(error) => eprintln!("shot {name} skipped: {error}"),
        }
    }

    /// Compares where the layout put each visible pane with the native views
    /// on screen: every visible pane has a shown view at exactly its slot,
    /// and no other view is shown.
    fn views_match_layout(&self, cx: &mut Cx) -> Result<usize, String> {
        cx.update(|window, cx| {
            let area = self.area.read(cx).hooks();
            // Detached panes show a Reattach button instead of a view.
            let drawn = area.drawn();
            let slots: Vec<Bounds<Pixels>> = drawn
                .panes
                .iter()
                .filter(|(pane, _)| !drawn.reattach.contains_key(*pane))
                .map(|(_, bounds)| *bounds)
                .collect();
            drop(drawn);
            if let Some(tiny) = slots
                .iter()
                .find(|slot| slot.size.width < px(80.) || slot.size.height < px(60.))
            {
                return Err(format!("a pane slot is too small to use: {tiny:?}"));
            }
            let shown: Vec<Bounds<Pixels>> = overlay::native_subviews(window)
                .into_iter()
                .filter(|(_, hidden)| !hidden)
                .map(|(bounds, _)| bounds)
                .collect();
            let near = |a: &Bounds<Pixels>, b: &Bounds<Pixels>| {
                let d = |x: Pixels, y: Pixels| (f32::from(x) - f32::from(y)).abs() < 1.0;
                d(a.origin.x, b.origin.x)
                    && d(a.origin.y, b.origin.y)
                    && d(a.size.width, b.size.width)
                    && d(a.size.height, b.size.height)
            };
            let missing: Vec<_> = slots
                .iter()
                .filter(|slot| !shown.iter().any(|view| near(slot, view)))
                .collect();
            let extra: Vec<_> = shown
                .iter()
                .filter(|view| !slots.iter().any(|slot| near(slot, view)))
                .collect();
            if missing.is_empty() && extra.is_empty() {
                Ok(slots.len())
            } else {
                Err(format!(
                    "slots without a view: {missing:?}; views without a slot: {extra:?}"
                ))
            }
        })
        .map_err(|e| e.to_string())?
    }

    fn check_views(&mut self, cx: &mut Cx, what: &str) {
        match self.views_match_layout(cx) {
            Ok(count) => eprintln!("ok   {what}: {count} views on their slots"),
            Err(error) => self.fail(&format!("{what}: {error}")),
        }
    }

    /// Dispatches `action` where the keyboard is: the overlay while
    /// something modal is open there, else the main window.
    fn dispatch(&self, cx: &mut Cx, action: Box<dyn Action>) {
        cx.update(|window, cx| overlay::dispatch_action(action, window, cx))
            .ok();
    }

    /// Types `text` one key press at a time, where the keyboard is.
    fn type_text(&self, cx: &mut Cx, text: &str) {
        cx.update(|window, cx| {
            for keystroke in overlay::keystrokes(text) {
                overlay::dispatch_keystroke(keystroke, window, cx);
            }
        })
        .ok();
    }

    fn mouse(&self, cx: &mut Cx, event: PlatformInput) {
        cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        })
        .ok();
    }

    /// The tab the script works in and its tree.
    fn tab(&self, cx: &mut Cx) -> anyhow::Result<Option<(TabId, SplitTree)>> {
        self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            Hooks::active_tab(&snapshot)
                .and_then(|tab| Some((tab.info.tab_id.clone(), tab.root.clone()?)))
        })
    }

    async fn read_pane_text(&self, cx: &mut Cx, pane_id: &PaneId) -> String {
        let pane_id = pane_id.to_owned();
        let Ok(task) = self.read(cx, |area, cx| {
            area.state()
                .read(cx)
                .call(&area.machine(cx), cx, move |client| {
                    client.send::<method::PaneRead>(&protocol::PaneReadParams {
                        pane_id,
                        source: protocol::ReadSource::Recent,
                        lines: Some(40),
                    })
                })
        }) else {
            return String::new();
        };
        match task.await {
            Ok(read) => read.text,
            Err(error) => format!("<{error}>"),
        }
    }
}

/// Runs the capture helper, giving up after `timeout`: ScreenCaptureKit can
/// hang (seen with several captures queued machine-wide), and the script
/// must not stall on a screenshot.
fn capture(mut command: std::process::Command, timeout: Duration) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    let mut child = command
        // Its own process group: `swift` runs the script in a child
        // `swift-frontend`, which a timeout must stop too.
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    std::io::Read::read_to_string(&mut pipe, &mut stderr).ok();
                }
                return Err(stderr.trim().to_owned());
            }
            Ok(None) if start.elapsed() > timeout => {
                // Our own child's group, nothing else.
                std::process::Command::new("kill")
                    .arg("-KILL")
                    .arg(format!("-{}", child.id()))
                    .status()
                    .ok();
                child.wait().ok();
                return Err(format!("capture timed out after {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn center(bounds: Bounds<Pixels>) -> Point<Pixels> {
    bounds.origin + point(bounds.size.width / 2., bounds.size.height / 2.)
}

impl Run {
    fn press(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
        );
    }

    fn release(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
        );
    }

    async fn click(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.press(cx, at);
        Self::settle(cx).await;
        self.release(cx, at);
        Self::settle(cx).await;
    }

    /// Drags with the left button from `from` to `to` in `steps` frames;
    /// `during` runs at the halfway step, while the button is still down.
    async fn drag(
        &mut self,
        cx: &mut Cx,
        from: Point<Pixels>,
        to: Point<Pixels>,
        steps: usize,
        mut during: impl FnMut(&mut Self, &mut Cx),
    ) {
        self.press(cx, from);
        Self::settle(cx).await;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let position = from + point((to.x - from.x) * t, (to.y - from.y) * t);
            self.mouse(
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
            );
            Self::settle(cx).await;
            if step == steps / 2 {
                during(self, cx);
            }
        }
        self.release(cx, to);
        Self::settle(cx).await;
    }

    fn drawn<R>(&self, cx: &mut Cx, f: impl FnOnce(&Drawn) -> R) -> anyhow::Result<R> {
        self.read(cx, |area, _| f(&area.drawn()))
    }

    fn focused_pane(&self, cx: &mut Cx) -> Option<PaneId> {
        self.read(cx, |area, cx| area.snapshot(cx).focused_pane_id.clone())
            .ok()
            .flatten()
    }
}
