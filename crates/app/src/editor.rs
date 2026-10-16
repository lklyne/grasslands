//! The code editor: file tabs and GPUI Kit's code editor, grouped per
//! worktree. Placeholder until Part 2.

use std::path::{Path, PathBuf};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::placeholder::placeholder;
use crate::state::{AppEvent, AppState};

pub fn init(_cx: &mut App) {}

pub struct Editor {
    state: Entity<AppState>,
    open: Vec<PathBuf>,
    _subscription: Subscription,
}

impl Editor {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_this, _, event, cx| {
            if let AppEvent::ActiveWorktreeChanged(_) = event {
                cx.notify();
            }
        });
        Self {
            state,
            open: Vec::new(),
            _subscription: subscription,
        }
    }

    pub fn open(&mut self, path: &Path, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.open.iter().any(|open| open == path) {
            self.open.push(path.to_path_buf());
        }
        cx.notify();
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let worktree = self.state.read(cx).active_worktree().cloned();
        let tabs = self.open.iter().map(|path| {
            div().px_2().text_xs().child(
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            )
        });
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(tabs),
            )
            .child(placeholder(
                "Editor",
                worktree.map_or_else(|| "no worktree".to_owned(), |w| w.display().to_string()),
                cx,
            ))
    }
}
