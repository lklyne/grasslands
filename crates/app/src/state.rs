//! `AppState`: the one shared model every view reads.
//!
//! It holds the herdr cache and the active worktree. Views keep an
//! `Entity<AppState>`, read it in `render`, and `cx.observe` it to re-render.
//! Discrete changes that views must react to (not just redraw) are emitted
//! as `AppEvent`s; views `cx.subscribe` to those.

use std::path::PathBuf;

use gpui_kit::*;

use crate::herdr::{self, ConnectionStatus, Snapshot};

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The focused pane's worktree root changed. The file tree re-roots and
    /// the editor swaps to that worktree's tab set.
    ActiveWorktreeChanged(Option<PathBuf>),
    /// A fresh herdr snapshot replaced the cache (startup or reconnect).
    SnapshotReplaced,
}

pub struct AppState {
    pub herdr_config: herdr::Config,
    pub connection: ConnectionStatus,
    pub snapshot: Snapshot,
    pub focused_pane: Option<String>,
    active_worktree: Option<PathBuf>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new(herdr_config: herdr::Config, _cx: &mut Context<Self>) -> Self {
        Self {
            herdr_config,
            connection: ConnectionStatus::default(),
            snapshot: Snapshot::default(),
            focused_pane: None,
            active_worktree: std::env::current_dir().ok(),
        }
    }

    pub fn active_worktree(&self) -> Option<&PathBuf> {
        self.active_worktree.as_ref()
    }

    pub fn set_active_worktree(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active_worktree == root {
            return;
        }
        self.active_worktree.clone_from(&root);
        cx.emit(AppEvent::ActiveWorktreeChanged(root));
        cx.notify();
    }

    pub fn replace_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.emit(AppEvent::SnapshotReplaced);
        cx.notify();
    }
}
