//! The file tree, rooted at the active worktree. Click sends `OpenFile`.
//! Placeholder until the files agent fills it.

use gpui_kit::*;

use crate::placeholder::placeholder;
use crate::state::{AppEvent, AppState};

pub fn init(_cx: &mut App) {}

pub struct FileTree {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl FileTree {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_this, _, event, cx| {
            if let AppEvent::ActiveWorktreeChanged(_) = event {
                cx.notify();
            }
        });
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.state.read(cx).active_worktree().map_or_else(
            || "no worktree".to_owned(),
            |root| root.display().to_string(),
        );
        placeholder("Files", root, cx)
    }
}
