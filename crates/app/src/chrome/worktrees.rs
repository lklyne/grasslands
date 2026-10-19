//! Worktree commands beyond create: Open Worktree… (a picker fed by
//! `worktree.list`, then `worktree.open` or a focus) and Remove Worktree…
//! (`worktree.remove`, asking again with `force` when the checkout is dirty).
//! Every call goes to the machine the command was built for.
//!
//! Which workspaces are linked worktrees, and which form a group, follows
//! what the spaces list shows: herdr's `WorkspaceInfo::worktree`, else what
//! the list read with git (see [`set_placements`]).

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::*;

use super::confirm::{self, Confirmation};
use super::{Chrome, HerdrCommand, update_later};
use crate::herdr::{self, Snapshot, WorkspaceInfo, WorktreeList, WorktreeParams};
use crate::sounds;
use crate::state::MachineId;
use crate::worktree::Placement;

/// herdr's code when a worktree has uncommitted or untracked files.
const DIRTY: &str = "dirty_worktree_requires_force";
/// herdr's code when the workspace isn't a linked worktree it can remove.
const NOT_LINKED: &str = "not_linked_worktree";

/// Where a workspace sits in its repo, as the spaces list shows it.
pub type PlaceFn = Rc<dyn Fn(&MachineId, &WorkspaceInfo, &App) -> Option<Placement>>;

struct Placements(PlaceFn);

impl Global for Placements {}

/// The spaces list registers how it places workspaces (herdr's info, else
/// git's), so menus, the palette and group closes agree with its rows.
pub fn set_placements(cx: &mut App, place: PlaceFn) {
    cx.set_global(Placements(place));
}

/// herdr's own placement of a workspace, nothing inferred.
pub(super) fn herdr_placement(info: &WorkspaceInfo) -> Option<Placement> {
    info.worktree.as_ref().map(Placement::from_herdr)
}

/// Where `info` sits, as the spaces list shows it; herdr's info alone when
/// no list is registered (headless tests).
pub(super) fn placement(machine: &MachineId, info: &WorkspaceInfo, cx: &App) -> Option<Placement> {
    match cx.try_global::<Placements>() {
        Some(Placements(place)) => place(machine, info, cx),
        None => herdr_placement(info),
    }
}

/// A worktree command from a menu or the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeCommand {
    /// Pick one of `workspace_id`'s repo's worktrees and open it.
    Open {
        machine: MachineId,
        workspace_id: String,
    },
    /// Remove the linked worktree `workspace_id` works in.
    Remove {
        machine: MachineId,
        workspace_id: String,
    },
}

/// One row of the Open Worktree picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PickerRow {
    /// The branch, with notes (main checkout, open, prunable).
    pub label: String,
    pub path: String,
    /// The workspace already showing it, which picking focuses.
    pub open_workspace_id: Option<String>,
    /// Prunable: git lost its folder, so there is nothing to open.
    pub disabled: bool,
}

/// The picker's rows for `list`, in herdr's order (main checkout first).
/// Bare entries have no checkout to open and are left out.
pub(super) fn picker_rows(list: &WorktreeList) -> Vec<PickerRow> {
    list.worktrees
        .iter()
        .filter(|w| !w.is_bare)
        .map(|w| {
            let name = match (&w.branch, w.is_detached) {
                (Some(branch), false) => branch.clone(),
                _ => "detached HEAD".to_owned(),
            };
            let mut notes = Vec::new();
            if !w.is_linked_worktree {
                notes.push("main checkout");
            }
            if w.open_workspace_id.is_some() {
                notes.push("open");
            }
            if w.is_prunable {
                notes.push("missing, prunable");
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join(", "))
            };
            PickerRow {
                label: format!("{name}{notes}"),
                path: w.path.clone(),
                open_workspace_id: w.open_workspace_id.clone(),
                disabled: w.is_prunable,
            }
        })
        .collect()
}

/// The question before removing `workspace_id`'s worktree: what goes, and
/// which agents stop.
pub(super) fn remove_confirmation(snapshot: &Snapshot, workspace_id: &str) -> Confirmation {
    let info = snapshot.workspace(workspace_id).map(|w| &w.info);
    let folder = info
        .and_then(|i| i.worktree.as_ref())
        .map(|wt| format!(" ({})", wt.checkout_path))
        .unwrap_or_default();
    let mut message = format!(
        "This deletes the worktree's folder{folder} and closes the workspace. \
         Its branch is kept."
    );
    let names = confirm::agents_in(snapshot, &[workspace_id]);
    if !names.is_empty() {
        message.push(' ');
        message.push_str(&confirm::agent_summary(&names));
    }
    Confirmation {
        title: info.map_or_else(
            || "Remove this worktree?".to_owned(),
            |i| format!("Remove worktree {}?", i.label),
        ),
        message,
        confirm_label: "Remove Worktree".into(),
    }
}

/// The second question, after herdr refused a dirty checkout.
pub(super) fn force_confirmation(label: &str, refusal: &str) -> Confirmation {
    Confirmation {
        title: format!("Delete uncommitted changes in {label}?"),
        message: format!(
            "{label} has uncommitted or untracked files. Removing it anyway deletes \
             them for good, with the worktree's folder. git said: {refusal}"
        ),
        confirm_label: "Remove Anyway".into(),
    }
}

impl Chrome {
    pub(super) fn run_worktree_command(
        &mut self,
        command: WorktreeCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WorktreeCommand::Open {
                machine,
                workspace_id,
            } => self.open_worktree_picker(machine, workspace_id, window, cx),
            WorktreeCommand::Remove {
                machine,
                workspace_id,
            } => self.remove_worktree(machine, workspace_id, window, cx),
        }
    }

    /// Lists `workspace_id`'s repo's worktrees, then shows them in a picker.
    fn open_worktree_picker(
        &mut self,
        machine: MachineId,
        workspace_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(window, cx);
        let task = self
            .state
            .read(cx)
            .list_worktrees_on(&machine, &workspace_id, cx);
        cx.spawn(async move |this, cx| {
            let list = match task.await {
                Ok(list) => list,
                Err(error) => {
                    sounds::play(cues::Cue::Error, cues::PlayOptions::default());
                    this.update(cx, |chrome, cx| {
                        chrome.error_toast("Open worktree", error.to_string(), cx)
                    })
                    .ok();
                    return;
                }
            };
            this.update(cx, |chrome, cx| {
                chrome.show_worktree_picker(machine, workspace_id, list, cx)
            })
            .ok();
        })
        .detach();
    }

    fn show_worktree_picker(
        &mut self,
        machine: MachineId,
        workspace_id: String,
        list: WorktreeList,
        cx: &mut Context<Self>,
    ) {
        let rows = Rc::new(picker_rows(&list));
        let repo = list.source.repo_name;
        let chrome = cx.entity();
        sounds::play(cues::Cue::Open, sounds::subtle());
        self.floating(cx, move |window, cx| {
            let command_state = cx.new(|cx| CommandState::new(window, cx));
            chrome.update(cx, |chrome, _| {
                chrome.palette = Some(command_state.downgrade());
            });
            let first_frame = Cell::new(true);
            let chrome = chrome.downgrade();
            window.open_dialog(cx, move |dialog, window, cx| {
                if first_frame.replace(false) {
                    command_state.update(cx, |picker, cx| picker.focus(window, cx));
                }
                let theme = cx.theme();
                let (mono, muted) = (theme.mono_font_family.clone(), theme.muted_foreground);
                let items = rows.iter().map(|row| {
                    let label: SharedString = row.label.clone().into();
                    let path: SharedString = row.path.clone().into();
                    let mono = mono.clone();
                    CommandItem::new()
                        .label(label.clone())
                        .keywords([path.clone()])
                        .disabled(row.disabled)
                        .child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    Icon::new(assets::IconName::GitBranch)
                                        .small()
                                        .text_color(muted),
                                )
                                .child(div().flex_shrink_0().child(label.clone()))
                                .child(
                                    div()
                                        .ml_auto()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(mono.clone())
                                        .text_xs()
                                        .text_color(muted)
                                        .child(path.clone()),
                                )
                        })
                });
                let (rows, chrome) = (rows.clone(), chrome.clone());
                let (machine, source) = (machine.clone(), workspace_id.clone());
                let picker = Command::new(&command_state)
                    .bordered(false)
                    .max_h(px(420.))
                    .placeholder(format!("Open a worktree of {repo}…"))
                    .group(CommandGroup::new().label("Worktrees").items(items))
                    .on_confirm(move |index, window, cx| {
                        window.close_dialog(cx);
                        let Some(row) = rows.get(index.row).cloned() else {
                            return;
                        };
                        let (machine, source) = (machine.clone(), source.clone());
                        update_later(&chrome, cx, move |chrome, window, cx| {
                            chrome.open_worktree(machine, source, row, window, cx);
                        });
                    })
                    .on_cancel(|window, cx| {
                        sounds::play(cues::Cue::Close, sounds::subtle());
                        window.close_dialog(cx);
                    });
                dialog.w(px(600.)).p_0().close_button(false).child(picker)
            });
        });
    }

    /// Focuses the workspace already showing `row`, or opens it with
    /// `worktree.open` as a new, focused workspace.
    fn open_worktree(
        &mut self,
        machine: MachineId,
        source_workspace_id: String,
        row: PickerRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace_id) = row.open_workspace_id {
            let command = HerdrCommand::FocusWorkspace {
                machine,
                workspace_id,
            };
            self.run(command, window, cx);
            return;
        }
        let task = self.state.update(cx, |state, cx| {
            state.select_machine(&machine, cx);
            let params = WorktreeParams {
                workspace_id: Some(source_workspace_id),
                path: Some(row.path),
                focus: true,
                ..WorktreeParams::default()
            };
            state.open_worktree_on(&machine, params, cx)
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(_) => sounds::play(cues::Cue::Success, cues::PlayOptions::default()),
            Err(error) => {
                sounds::play(cues::Cue::Error, cues::PlayOptions::default());
                this.update(cx, |chrome, cx| {
                    chrome.error_toast("Open worktree", error.to_string(), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks, then removes `workspace_id`'s worktree.
    fn remove_worktree(
        &mut self,
        machine: MachineId,
        workspace_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let confirmation = remove_confirmation(self.snapshot_of(&machine, cx), &workspace_id);
        let chrome = cx.entity().downgrade();
        self.confirm(
            confirmation,
            move |_, cx| {
                let (machine, workspace_id) = (machine.clone(), workspace_id.clone());
                update_later(&chrome, cx, move |chrome, _, cx| {
                    chrome.send_remove(machine, workspace_id, false, cx);
                });
            },
            window,
            cx,
        );
    }

    /// `worktree.remove`; a dirty checkout asks again before forcing.
    fn send_remove(
        &mut self,
        machine: MachineId,
        workspace_id: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let label = self
            .snapshot_of(&machine, cx)
            .workspace(&workspace_id)
            .map_or_else(|| workspace_id.clone(), |w| w.info.label.clone());
        let task = self
            .state
            .read(cx)
            .remove_worktree_on(&machine, &workspace_id, force, cx);
        cx.spawn(async move |this, cx| {
            let error = match task.await {
                Ok(_) => {
                    sounds::play(cues::Cue::Close, sounds::strong());
                    return;
                }
                Err(error) => error,
            };
            this.update(cx, |chrome, cx| {
                chrome.remove_failed(machine, workspace_id, &label, force, &error, cx)
            })
            .ok();
        })
        .detach();
    }

    fn remove_failed(
        &mut self,
        machine: MachineId,
        workspace_id: String,
        label: &str,
        forced: bool,
        error: &herdr::Error,
        cx: &mut Context<Self>,
    ) {
        match (error, forced) {
            (herdr::Error::Server { code, message }, false) if code == DIRTY => {
                sounds::play(cues::Cue::Warning, cues::PlayOptions::default());
                let refusal = message.trim().trim_start_matches("fatal: ");
                let confirmation = force_confirmation(label, refusal);
                let chrome = cx.entity().downgrade();
                let main = self.main_window;
                // Through the main window: this runs from a task, not a view.
                cx.defer(move |cx| {
                    cx.update_window(main, |_, window, cx| {
                        let Some(this) = chrome.upgrade() else {
                            return;
                        };
                        let chrome = chrome.clone();
                        this.update(cx, |this, cx| {
                            this.confirm(
                                confirmation,
                                move |_, cx| {
                                    let (machine, workspace_id) =
                                        (machine.clone(), workspace_id.clone());
                                    update_later(&chrome, cx, move |chrome, _, cx| {
                                        chrome.send_remove(machine, workspace_id, true, cx);
                                    });
                                },
                                window,
                                cx,
                            );
                        });
                    })
                    .ok();
                });
            }
            (herdr::Error::Server { code, .. }, _) if code == NOT_LINKED => {
                sounds::play(cues::Cue::Error, sounds::subtle());
                let message = format!(
                    "herdr doesn't treat {label} as a linked worktree, so it can't remove it. \
                     Close the workspace instead, or run git worktree remove yourself."
                );
                self.floating(cx, move |window, cx| {
                    window.push_notification(
                        Notification::new()
                            .with_type(NotificationType::Warning)
                            .title("Remove worktree")
                            .message(message),
                        cx,
                    );
                });
            }
            _ => {
                sounds::play(cues::Cue::Error, cues::PlayOptions::default());
                self.error_toast("Remove worktree", error.to_string(), cx);
            }
        }
    }

    fn error_toast(&mut self, title: &'static str, message: String, cx: &mut Context<Self>) {
        self.floating(cx, move |window, cx| {
            window.push_notification(Notification::error(message).title(title), cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{PickerRow, force_confirmation, picker_rows, remove_confirmation};
    use crate::herdr::{Snapshot, WorktreeInfo, WorktreeList};

    fn worktree(path: &str, branch: Option<&str>, linked: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: path.into(),
            label: "repo".into(),
            is_bare: false,
            is_detached: branch.is_none(),
            is_prunable: false,
            is_linked_worktree: linked,
            branch: branch.map(str::to_owned),
            open_workspace_id: None,
        }
    }

    fn list(worktrees: Vec<WorktreeInfo>) -> WorktreeList {
        serde_json::from_value(serde_json::json!({
            "source": {
                "repo_key": "/r/.git",
                "repo_name": "repo",
                "repo_root": "/r",
                "source_checkout_path": "/r",
            },
            "worktrees": worktrees,
        }))
        .unwrap()
    }

    #[test]
    fn picker_marks_the_main_checkout() {
        let rows = picker_rows(&list(vec![worktree("/r", Some("main"), false)]));
        assert_eq!(rows[0].label, "main (main checkout)");
    }

    #[test]
    fn picker_keeps_the_open_workspace_to_focus() {
        let mut open = worktree("/w/a", Some("a"), true);
        open.open_workspace_id = Some("w3".into());
        let rows = picker_rows(&list(vec![open]));
        assert_eq!(
            rows,
            vec![PickerRow {
                label: "a (open)".into(),
                path: "/w/a".into(),
                open_workspace_id: Some("w3".into()),
                disabled: false,
            }]
        );
    }

    #[test]
    fn picker_names_a_detached_checkout() {
        let rows = picker_rows(&list(vec![worktree("/w/d", None, true)]));
        assert_eq!(rows[0].label, "detached HEAD");
    }

    #[test]
    fn picker_skips_bare_entries() {
        let mut bare = worktree("/r.git", None, false);
        bare.is_bare = true;
        let rows = picker_rows(&list(vec![bare, worktree("/w/a", Some("a"), true)]));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn picker_disables_prunable_entries() {
        let mut gone = worktree("/w/gone", Some("gone"), true);
        gone.is_prunable = true;
        let rows = picker_rows(&list(vec![gone]));
        assert!(rows[0].disabled);
    }

    #[test]
    fn remove_confirmation_without_the_workspace_asks_generically() {
        let confirmation = remove_confirmation(&Snapshot::default(), "w9");
        assert_eq!(confirmation.title, "Remove this worktree?");
    }

    #[test]
    fn force_confirmation_quotes_git() {
        let confirmation = force_confirmation("feat", "contains modified files");
        assert!(
            confirmation
                .message
                .ends_with("git said: contains modified files")
        );
    }
}
