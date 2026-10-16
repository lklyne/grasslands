//! `--script`: drives the spike through every question with GPUI events
//! dispatched inside the app (never through the OS), reads state back and
//! screenshots this app's window alone into `shots/`.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::{FRAME_INSET, Spike, TOOLBAR_HEIGHT, herdr, native};

type Cx = AsyncApp;

struct Run {
    window: AnyWindowHandle,
    spike: Entity<Spike>,
    number: isize,
    shot: u32,
    passed: u32,
    failed: Vec<String>,
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

async fn wait(cx: &mut Cx, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

fn keystroke(text: &str) -> Keystroke {
    Keystroke::parse(text).unwrap_or_else(|_| Keystroke {
        modifiers: Modifiers::default(),
        key: text.to_owned(),
        key_char: None,
    })
}

impl Run {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let detail = detail.as_ref();
        if ok {
            self.passed += 1;
            eprintln!("PASS {name}: {detail}");
        } else {
            self.failed.push(name.to_owned());
            eprintln!("FAIL {name}: {detail}");
        }
    }

    fn update<R>(&self, cx: &mut Cx, f: impl FnOnce(&mut Window, &mut App) -> R) -> Option<R> {
        self.window.update(cx, |_, window, cx| f(window, cx)).ok()
    }

    fn spike<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut Spike, &mut Window, &mut Context<Spike>) -> R,
    ) -> Option<R> {
        let spike = self.spike.clone();
        self.update(cx, |window, cx| {
            spike.update(cx, |spike, cx| f(spike, window, cx))
        })
    }

    fn keys(&self, cx: &mut Cx, keys: &[&str]) {
        self.update(cx, |window, cx| {
            for key in keys {
                window.dispatch_keystroke(keystroke(key), cx);
            }
        });
    }

    fn type_text(&self, cx: &mut Cx, text: &str) {
        self.update(cx, |window, cx| {
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
        });
    }

    fn mouse(&self, cx: &mut Cx, event: PlatformInput) {
        self.update(cx, |window, cx| {
            window.dispatch_event(event, cx);
        });
    }

    fn click(&self, cx: &mut Cx, button: MouseButton, position: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
        );
    }

    /// A left-button drag in `steps` moves, one GPUI frame apart, calling
    /// `each` after every move has been drawn.
    async fn drag(
        &mut self,
        cx: &mut Cx,
        from: Point<Pixels>,
        to: Point<Pixels>,
        steps: u32,
        modifiers: Modifiers,
        mut each: impl FnMut(&mut Self, &mut Cx, u32),
    ) {
        self.mouse(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: from,
                pressed_button: None,
                modifiers,
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }),
        );
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let position = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.mouse(
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers,
                }),
            );
            self.frame(cx).await;
            each(self, cx, step);
        }
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                modifiers,
                click_count: 1,
            }),
        );
    }

    /// Draws a frame now (the display link stops while the screen is locked)
    /// and gives Ghostty a moment.
    async fn frame(&self, cx: &mut Cx) {
        self.update(cx, |window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        wait(cx, 16).await;
    }

    fn shoot(&mut self, name: &str) {
        self.shot += 1;
        let dir = manifest().join("shots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{:02}-{name}.png", self.shot));
        let tool = manifest().join("target/capture-windows");
        let status = std::process::Command::new(tool)
            .arg(&path)
            .arg(self.number.to_string())
            .status();
        match status {
            Ok(status) if status.success() => eprintln!("shot {}", path.display()),
            other => eprintln!("shot {name} skipped: {other:?}"),
        }
    }

    /// Starts a capture and returns at once, so the shot lands while the
    /// script keeps changing the window.
    fn shoot_async(&mut self, name: &str) -> Option<std::process::Child> {
        self.shot += 1;
        let path = manifest()
            .join("shots")
            .join(format!("{:02}-{name}.png", self.shot));
        eprintln!("shot {} (async)", path.display());
        std::process::Command::new(manifest().join("target/capture-windows"))
            .arg(&path)
            .arg(self.number.to_string())
            .spawn()
            .ok()
    }

    fn placement(&self, cx: &mut Cx) -> Option<native::Placement> {
        self.update(cx, |window, _| native::placement(window))
            .flatten()
    }

    /// The terminal views' frames against the slots the layout gave them.
    fn views_on_slots(&self, cx: &mut Cx, right_shown: bool) -> Result<String, String> {
        let left = self
            .spike(cx, |spike, _, cx| spike.left_width(cx))
            .unwrap_or(0.);
        let size = self
            .update(cx, |window, _| window.viewport_size())
            .unwrap_or_default();
        let placement = self.placement(cx).ok_or("no placement")?;
        let [l, r] = placement.below[..] else {
            return Err(format!("{} views below GPUI", placement.below.len()));
        };
        let height = f64::from(f32::from(size.height)) - f64::from(TOOLBAR_HEIGHT);
        let width = f64::from(f32::from(size.width));
        let close = |a: f64, b: f64| (a - b).abs() <= 1.5;
        let left_ok = close(l.x, 0.)
            && close(l.y, f64::from(TOOLBAR_HEIGHT))
            && close(l.height, height)
            && if right_shown {
                close(l.width, f64::from(left))
            } else {
                close(l.width, width)
            }
            && !l.hidden;
        let right_ok = if right_shown {
            close(r.x, f64::from(left)) && close(r.x + r.width, width) && !r.hidden
        } else {
            r.hidden
        };
        let detail = format!("divider={left:.1} window={width}x{height} left={l:?} right={r:?}");
        if left_ok && right_ok {
            Ok(detail)
        } else {
            Err(detail)
        }
    }

    fn snapshot_hash(&self, cx: &mut Cx, right: bool) -> Option<u64> {
        self.spike(cx, |spike, _, cx| {
            let terminal = if right { &spike.right } else { &spike.left };
            let image = terminal
                .update(cx, |terminal, _| terminal.snapshot())
                .ok()?;
            let mut hasher = DefaultHasher::new();
            image.as_bytes(0)?.hash(&mut hasher);
            Some(hasher.finish())
        })
        .flatten()
    }

    /// A point inside the left terminal, `row` text rows from its top.
    fn left_point(x: f32, y: f32) -> Point<Pixels> {
        point(px(FRAME_INSET + x), px(TOOLBAR_HEIGHT + FRAME_INSET + y))
    }
}

pub async fn run(window: AnyWindowHandle, spike: Entity<Spike>, cx: &mut Cx) {
    wait(cx, 2500).await;
    let Ok(Some(number)) = window.update(cx, |_, window, _| native::window_number(window)) else {
        eprintln!("script: no window number");
        return;
    };
    let mut run = Run {
        window,
        spike,
        number,
        shot: 0,
        passed: 0,
        failed: Vec::new(),
    };
    let saved_clipboard = cx.update(|cx| cx.read_from_clipboard());
    steps(&mut run, cx).await;
    if let Some(item) = saved_clipboard {
        cx.update(|cx| cx.write_to_clipboard(item));
    }
    eprintln!(
        "script: {} passed, {} failed {:?}",
        run.passed,
        run.failed.len(),
        run.failed
    );
    if std::env::args().any(|arg| arg == "--quit") {
        cx.update(|cx| cx.quit());
    }
}

async fn steps(run: &mut Run, cx: &mut Cx) {
    // 1. The views are below GPUI, in their slots.
    run.frame(cx).await;
    let placement = run.placement(cx);
    let detail = placement.as_ref().map(|p| {
        format!(
            "below={} above={} contentView order={:?}",
            p.below.len(),
            p.above,
            p.order
        )
    });
    let order_ok = placement.as_ref().is_some_and(|p| {
        p.above == 0
            && p.below.len() == 2
            && p.order.iter().position(|c| c == "GPUIView")
                > p.order.iter().rposition(|c| c == "GpuiGhosttyView")
    });
    run.check("views below GPUIView", order_ok, detail.unwrap_or_default());
    let slots = run.views_on_slots(cx, true);
    run.check(
        "views on their slots",
        slots.is_ok(),
        slots.unwrap_or_else(|e| e),
    );
    let (windows, key) = run
        .update(cx, |w, _| native::window_state(w))
        .unwrap_or_default();
    run.check(
        "one window, key",
        windows == 1 && key,
        format!("windows={windows} key={key}"),
    );
    run.shoot("initial");

    // 2. Typing into the focused left terminal through GPUI's key dispatch.
    run.type_text(cx, "clear; echo typed-into-left-$((6*7))\n");
    wait(cx, 1200).await;
    let text = herdr::read_pane(0);
    run.check(
        "typing reaches the terminal",
        text.contains("typed-into-left-42"),
        "pane 0 shows typed-into-left-42",
    );
    run.shoot("typed");

    // 3. Divider drag: the views follow the layout on every frame.
    let start = run.spike(cx, |s, _, cx| s.left_width(cx)).unwrap_or(560.);
    let from = point(px(start), px(TOOLBAR_HEIGHT + 300.));
    let to = point(px(start - 260.), px(TOOLBAR_HEIGHT + 300.));
    let mut misplaced = Vec::new();
    let mut frame_times = Vec::new();
    let started = Instant::now();
    run.drag(cx, from, to, 40, Modifiers::default(), |run, cx, step| {
        let begun = Instant::now();
        if let Err(detail) = run.views_on_slots(cx, true) {
            misplaced.push(format!("step {step}: {detail}"));
        }
        frame_times.push(begun.elapsed());
        if step == 20 {
            run.shoot("mid-drag");
        }
    })
    .await;
    let elapsed = started.elapsed();
    run.frame(cx).await;
    let end = run.spike(cx, |s, _, cx| s.left_width(cx)).unwrap_or(0.);
    run.check(
        "divider drag moved the split",
        (end - (start - 260.)).abs() < 3.,
        format!("{start:.0} -> {end:.0}"),
    );
    run.check(
        "views on slots every drag step",
        misplaced.is_empty(),
        format!("40 steps in {elapsed:?}; misplaced: {misplaced:?}"),
    );
    wait(cx, 400).await;
    run.shoot("after-drag");

    // 4a. A menu over the right terminal takes the keyboard in this window.
    let before = herdr::read_pane(0);
    run.keys(cx, &["cmd-2"]);
    run.frame(cx).await;
    let menu_focused = run
        .spike(cx, |s, window, cx| {
            s.menu
                .as_ref()
                .map(|m| m.view.focus_handle(cx).contains_focused(window, cx))
        })
        .flatten();
    run.check(
        "menu open and focused",
        menu_focused == Some(true),
        format!("{menu_focused:?}"),
    );
    wait(cx, 300).await;
    run.shoot("menu-over-terminal");
    run.keys(cx, &["down", "down", "enter"]);
    run.frame(cx).await;
    let picked = run.spike(cx, |s, _, _| s.picked.clone()).flatten();
    run.check(
        "menu keyboard pick",
        picked.as_deref() == Some("Rename pane"),
        format!("{picked:?}"),
    );
    let closed = run.spike(cx, |s, _, _| s.menu.is_none()).unwrap_or(false);
    run.check("menu closed", closed, "");
    wait(cx, 600).await;
    let after = herdr::read_pane(0);
    run.check(
        "menu keys did not reach the terminal",
        before == after,
        "pane 0 unchanged",
    );

    // 4b. A dialog with a text input over both terminals.
    run.keys(cx, &["cmd-3"]);
    run.frame(cx).await;
    run.frame(cx).await;
    run.type_text(cx, "hello dialog");
    run.frame(cx).await;
    let value = run
        .spike(cx, |s, _, cx| {
            s.dialog_input
                .as_ref()
                .map(|i| i.read(cx).value().to_string())
        })
        .flatten();
    run.check(
        "dialog input typed",
        value.as_deref() == Some("hello dialog"),
        format!("{value:?}"),
    );
    let (windows, key) = run
        .update(cx, |w, _| native::window_state(w))
        .unwrap_or_default();
    run.check(
        "still one window, still key",
        windows == 1 && key,
        format!("windows={windows} key={key}"),
    );
    wait(cx, 600).await;
    run.shoot("dialog-over-terminals");
    let leaked = herdr::read_pane(0).contains("hello dialog")
        || herdr::read_pane(1).contains("hello dialog");
    run.check("dialog keys did not reach a terminal", !leaked, "");
    run.keys(cx, &["escape"]);
    run.frame(cx).await;
    // Clicking the terminal hands the keyboard back to it.
    run.click(cx, MouseButton::Left, Run::left_point(40., 200.));
    run.frame(cx).await;
    run.type_text(cx, "echo back-in-terminal\n");
    wait(cx, 900).await;
    run.check(
        "typing after the dialog",
        herdr::read_pane(0).contains("back-in-terminal"),
        "",
    );

    // 4c. A toast.
    run.keys(cx, &["cmd-4"]);
    run.frame(cx).await;
    wait(cx, 500).await;
    run.shoot("toast");

    // 3b. Selection by drag, cmd-c, cmd-v.
    run.type_text(cx, "clear; echo pastemarker-abc\n");
    wait(cx, 1000).await;
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
    // Row 0 after `clear` holds the echo's output.
    let active = run.update(cx, |w, _| w.is_window_active());
    eprintln!("select: window active={active:?}");
    // A plain drag first, to see what herdr does with it.
    run.drag(
        cx,
        Run::left_point(1., 7.),
        Run::left_point(400., 7.),
        12,
        Modifiers::default(),
        |_, _, _| {},
    )
    .await;
    run.frame(cx).await;
    wait(cx, 300).await;
    run.shoot("plain-drag");
    run.click(cx, MouseButton::Left, Run::left_point(40., 300.));
    // herdr's attach client turns on mouse reporting, and Ghostty then hands
    // plain drags to herdr instead of selecting; shift overrides that, as in
    // Ghostty.app.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    run.drag(
        cx,
        Run::left_point(1., 7.),
        Run::left_point(400., 7.),
        12,
        shift,
        |run, _, step| {
            if step == 12 {
                run.shoot("selecting");
            }
        },
    )
    .await;
    run.frame(cx).await;
    run.keys(cx, &["cmd-c"]);
    for _ in 0..20 {
        run.frame(cx).await;
    }
    wait(cx, 300).await;
    run.frame(cx).await;
    let copied = cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap_or_default();
    run.check(
        "drag-select + cmd-c copies",
        copied.trim() == "pastemarker-abc",
        format!("{copied:?}"),
    );
    run.shoot("selection");
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("echo pasted-$((2+3))".to_owned()))
    });
    run.keys(cx, &["cmd-v"]);
    for _ in 0..20 {
        run.frame(cx).await;
    }
    run.type_text(cx, "\n");
    wait(cx, 900).await;
    run.check("cmd-v pastes", herdr::read_pane(0).contains("pasted-5"), "");

    // 3c. Scrolling back through Ghostty's scrollback.
    run.type_text(cx, "seq 1 400\n");
    wait(cx, 1200).await;
    let before = run.snapshot_hash(cx, false);
    run.mouse(
        cx,
        PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: Run::left_point(100., 200.),
            delta: ScrollDelta::Lines(point(0., 20.)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        }),
    );
    for _ in 0..10 {
        run.frame(cx).await;
    }
    wait(cx, 300).await;
    let after = run.snapshot_hash(cx, false);
    run.check(
        "scroll changes what the terminal shows",
        before.is_some() && before != after,
        format!("{before:?} -> {after:?}"),
    );
    run.shoot("scrolled");
    // Back to the bottom.
    run.type_text(cx, "clear\n");
    wait(cx, 500).await;

    // 3d. Right-click reaches an app that turned on mouse reporting.
    let right_x = run.spike(cx, |s, _, cx| s.left_width(cx)).unwrap_or(300.) + 100.;
    run.click(
        cx,
        MouseButton::Left,
        point(px(right_x), px(TOOLBAR_HEIGHT + 200.)),
    );
    run.frame(cx).await;
    run.type_text(cx, "clear; printf '\\033[?1000h'; cat -v\n");
    wait(cx, 900).await;
    run.click(
        cx,
        MouseButton::Right,
        point(px(right_x + 40.), px(TOOLBAR_HEIGHT + 120.)),
    );
    run.frame(cx).await;
    run.type_text(cx, "\n");
    wait(cx, 900).await;
    let right = herdr::read_pane(1);
    run.check(
        "right-click reported to the app",
        right.contains("^[[M\""),
        format!(
            "pane 1: {:?}",
            right
                .lines()
                .filter(|l| l.contains("^["))
                .collect::<Vec<_>>()
        ),
    );
    run.shoot("mouse-report");
    run.keys(cx, &["ctrl-c"]);
    run.type_text(cx, "printf '\\033[?1000l'; clear\n");
    wait(cx, 500).await;

    // 2b. Hide the right pane, window resize, show it again.
    run.keys(cx, &["cmd-1"]);
    run.frame(cx).await;
    let hidden = run.views_on_slots(cx, false);
    run.check(
        "right hidden, left fills",
        hidden.is_ok(),
        hidden.unwrap_or_else(|e| e),
    );
    run.update(cx, |window, _| window.resize(size(px(1000.), px(600.))));
    wait(cx, 600).await;
    run.frame(cx).await;
    let hidden = run.views_on_slots(cx, false);
    run.check(
        "still right after window resize",
        hidden.is_ok(),
        hidden.unwrap_or_else(|e| e),
    );
    run.keys(cx, &["cmd-1"]);
    run.frame(cx).await;
    run.frame(cx).await;
    let shown = run.views_on_slots(cx, true);
    run.check(
        "right shown on its slot",
        shown.is_ok(),
        shown.unwrap_or_else(|e| e),
    );
    wait(cx, 400).await;
    run.shoot("resized");

    // 5a. Flicker: a burst of window resizes, one per frame, with a capture
    // started in the middle. Any strip neither GPUI nor Ghostty has drawn
    // shows the window backdrop (the left terminal's background), never the
    // desktop.
    let mut capture = None;
    for step in 0..60u32 {
        let t = step as f32 / 60.;
        let width = 1000. + 300. * (t * std::f32::consts::TAU).sin();
        run.update(cx, |window, _| window.resize(size(px(width), px(640.))));
        run.frame(cx).await;
        if step == 20 {
            capture = run.shoot_async("mid-window-resize");
        }
        let placed = run.views_on_slots(cx, true);
        if let Err(detail) = placed {
            eprintln!("resize step {step}: {detail}");
        }
    }
    if let Some(mut child) = capture {
        let _ = child.wait();
    }

    // 5. GPUI's frame cost with the transparent layer.
    let mut draws = Vec::new();
    for _ in 0..60 {
        let begun = Instant::now();
        run.update(cx, |window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        draws.push(begun.elapsed());
        wait(cx, 5).await;
    }
    draws.sort();
    eprintln!(
        "perf: window.draw median {:?} p90 {:?} (transparent layer, 2 holes)",
        draws[draws.len() / 2],
        draws[draws.len() * 9 / 10]
    );
}
