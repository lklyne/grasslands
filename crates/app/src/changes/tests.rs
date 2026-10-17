//! The Changes tab's diff in a headless window.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::v_flex;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, Modifiers, ParentElement as _, Point,
    Render, ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, TouchPhase,
    VisualTestContext, Window, WindowBounds, WindowOptions, point, px, size,
};

/// The diff laid out as `Changes` lays it out, recording the line under the
/// pointer.
struct View {
    diff: Entity<DiffState>,
    hovered: Rc<RefCell<Option<usize>>>,
}

impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let hovered = self.hovered.clone();
        v_flex().size_full().child(
            Diff::new(&self.diff)
                .flex_1()
                .min_h_0()
                .soft_wrap(true)
                .on_line_hover(move |line, _, _| {
                    if let Some(line) = line {
                        *hovered.borrow_mut() = Some(line.line());
                    }
                }),
        )
    }
}

/// Needs the vendored gpui-component patch (vendor/gpui-component/PATCHES.md):
/// without overdraw the wheel stops at the last painted row.
#[gpui_kit::test]
fn wheel_should_scroll_the_diff_past_the_first_screen(cx: &mut TestAppContext) {
    let lines: String = (1..=400).map(|i| format!("+line {i}\n")).collect();
    let patch =
        format!("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,400 @@\n{lines}");
    let hovered = Rc::new(RefCell::new(None));
    cx.update(gpui_kit::init);
    let window = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(800.), px(600.)),
            })),
            ..WindowOptions::default()
        };
        let hovered = hovered.clone();
        gpui_kit::open_window(options, cx, move |_, cx| {
            let diff = cx.new(|cx| DiffState::new(DiffFile::parse(&patch).unwrap(), cx));
            cx.new(|_| View { diff, hovered })
        })
        .unwrap()
        .0
    });
    let cx = &mut VisualTestContext::from_window(window, cx);
    cx.run_until_parked();
    let at = point(px(300.), px(300.));
    let line_at = |cx: &mut VisualTestContext| {
        // Leave and come back so the hover is reported afresh.
        cx.simulate_mouse_move(point(px(300.), px(590.)), None, Modifiers::none());
        cx.run_until_parked();
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.run_until_parked();
        hovered.borrow().expect("a line under the pointer")
    };
    let before = line_at(cx);
    for _ in 0..10 {
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.), px(-60.))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
    }
    let after = line_at(cx);
    assert!(
        after >= before + 20,
        "line under the pointer went from {before} to {after}"
    );
}
