//! `RT_SCRIPT=remote-files`: files on `dockerbox` (the `herdr-remote-test`
//! container) end to end, as 0020 describes. Selects its `api` workspace and
//! checks the file tree (ignored `target` hidden), footer and sidebar branch;
//! opens, edits and saves `src/main.rs`; changes it over ssh and waits for
//! the clean buffer to reload; searches; opens Changes; creates, renames and
//! deletes a file (through the permanent-delete confirmation); stops and
//! starts the container; and opens one path on Local and on dockerbox. Every
//! change on the machine is checked over plain ssh, and each step is
//! screenshotted to `target/script-shots/`.
//!
//! ```sh
//! GRASSLANDS_DEV_MACHINES=dockerbox RT_SCRIPT=remote-files scripts/dev-run cargo run -p app
//! ```
//!
//! At the end `~/api/src/main.rs` is put back as it was found and the
//! scratch files are removed. Stopping the container ends whatever else is
//! connected to it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use super::machines::{all_attached, docker, focus, remote, shows};
use super::titlebar::tab_titles;
use super::{Cx, Run};
use crate::actions::{OpenChanges, OpenFile};
use crate::herdr::{PaneId, method, protocol};
use crate::sidebar::{self, files::FileTree};
use crate::state::{AppState, MachineId, Status};

const CONTAINER: &str = "herdr-remote-test";
const SSH: [&str; 7] = [
    "-p",
    "2222",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "lyle@127.0.0.1",
];
const API: &str = "/home/lyle/api";
const SCRATCH: &str = "rf-scratch.txt";
const RENAMED: &str = "rf-renamed.txt";
/// A folder with the same canonical path on this Mac and in the container.
const SHARED: &str = "/Users/Shared";

impl Run {
    pub(super) async fn remote_files(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: remote-files");
        let connected = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                remote(area, cx)
                    .is_some_and(|m| m.status().is_usable() && m.snapshot().generation > 0)
            })
            .await;
        self.check(connected, "dockerbox connected");
        let Some(id) = self.read(cx, |area, cx| remote(area, cx).map(|m| m.id().clone()))? else {
            return Ok(());
        };
        if !connected {
            return Ok(());
        }
        let main_rs = format!("{API}/src/main.rs");
        let Some(original) = ssh(cx, &format!("cat {main_rs}"), None).await else {
            self.fail("read src/main.rs over ssh");
            return Ok(());
        };
        let status = ssh(cx, &format!("cd {API} && git status --porcelain"), None).await;
        eprintln!("script: ~/api before: {status:?}");
        let shared =
            PathBuf::from(SHARED).join(format!("grasslands-rf-{}.txt", std::process::id()));
        let result = self.remote_files_steps(cx, &id, &original, &shared).await;

        // Put ~/api back however the steps ended (a no-op start if running).
        docker(cx, &["start", CONTAINER]).await;
        let restored = ssh(cx, &format!("cat > {main_rs}"), Some(&original)).await;
        ssh(
            cx,
            &format!("rm -f {API}/{SCRATCH} {API}/{RENAMED} {}", shared.display()),
            None,
        )
        .await;
        let _ = std::fs::remove_file(&shared);
        // Only removes them if the script made them (they're empty then).
        docker_root(cx, &format!("rmdir {SHARED} /Users 2>/dev/null; true")).await;
        let after = ssh(cx, &format!("cd {API} && git status --porcelain"), None).await;
        let back = ssh(cx, &format!("cat {main_rs}"), None).await;
        self.check(
            restored.is_some() && back.as_deref() == Some(original.as_str()) && after == status,
            &format!("~/api is as it was ({after:?})"),
        );
        result
    }

    async fn remote_files_steps(
        &mut self,
        cx: &mut Cx,
        id: &MachineId,
        original: &str,
        shared: &Path,
    ) -> anyhow::Result<()> {
        let state = self.read(cx, |area, _| area.state().clone())?;
        let api_root = PathBuf::from(API);
        let api = self.read(cx, |area, cx| {
            area.state()
                .read(cx)
                .machine(id, cx)?
                .snapshot()
                .workspaces
                .iter()
                .find(|w| w.info.label == "api")
                .map(|w| w.info.workspace_id.clone())
        })?;
        let Some(api) = api else {
            self.fail("dockerbox has an api workspace");
            return Ok(());
        };

        // Select api; its pane's folder becomes the root.
        focus(cx, &state, id, &api)?;
        let shown = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                shows(area, cx, id, &api) && all_attached(area, cx)
            })
            .await;
        self.check(shown, "api on dockerbox is shown and attached");
        let at_api = |cx: &App, state: &Entity<AppState>| {
            state
                .read(cx)
                .active_root(cx)
                .is_some_and(|root| &root.machine == id && root.path == Path::new(API))
        };
        let mut rooted = self
            .until(cx, Duration::from_secs(10), |_, _, cx| at_api(cx, &state))
            .await;
        if !rooted {
            eprintln!("script: cd'ing api's pane into {API}");
            self.cd_focused_pane(cx, &state, API).await;
            rooted = self
                .until(cx, Duration::from_secs(20), |_, _, cx| at_api(cx, &state))
                .await;
        }
        let root = self.read(cx, |area, cx| area.state().read(cx).active_root(cx))?;
        self.check(
            rooted,
            &format!("the root is {API} on dockerbox ({root:?})"),
        );
        if !rooted {
            return Ok(());
        }

        // The tree, the footer and the sidebar row.
        let listed = self
            .until(cx, Duration::from_secs(10), |_, _, cx| {
                files(cx).is_some_and(|f| f.listed(&api_root).is_some())
            })
            .await;
        let names = self
            .files(cx, |f, _, _| f.listed(&api_root))?
            .unwrap_or_default();
        let want = [".gitignore", "README.md", "notes.txt", "src"];
        self.check(
            listed
                && want.iter().all(|w| names.iter().any(|n| n == w))
                && !names.iter().any(|n| n == "target"),
            &format!("the tree lists {want:?} and hides target ({names:?})"),
        );
        let footer = self
            .until(cx, Duration::from_secs(10), |_, _, cx| {
                files(cx)
                    .and_then(|f| f.git_summary().cloned())
                    .is_some_and(|g| g.branch.as_deref() == Some("feature") && g.diff.is_some())
            })
            .await;
        let git = self.files(cx, |f, _, _| {
            f.git_summary()
                .map(|g| (g.branch.clone(), g.parent.clone(), g.diff))
        })?;
        self.check(
            footer,
            &format!("the footer shows feature and +/- ({git:?})"),
        );
        let branch = self
            .until(cx, Duration::from_secs(15), |_, _, cx| {
                sidebar::spaces(cx).is_some_and(|s| {
                    s.read(cx).branch_of(id, &api, cx).as_deref() == Some("feature")
                })
            })
            .await;
        self.check(branch, "the sidebar row shows feature");
        Self::sleep(cx, 500).await;
        self.shoot(cx, "rf-tree");

        // Open src/main.rs.
        let src = api_root.join("src");
        let main_rs = src.join("main.rs");
        self.files(cx, |f, _, cx| f.expand(&src, cx))?;
        let expanded = self
            .until(cx, Duration::from_secs(10), |_, _, cx| {
                files(cx)
                    .and_then(|f| f.listed(&src))
                    .is_some_and(|names| names.iter().any(|n| n == "main.rs"))
            })
            .await;
        self.check(expanded, "expanding src lists main.rs");
        self.dispatch(
            cx,
            Box::new(OpenFile {
                path: main_rs.clone(),
                machine: id.clone(),
            }),
        );
        let opened = self
            .until(cx, Duration::from_secs(10), |_, window, cx| {
                editor_text(window, cx).is_some()
            })
            .await;
        let titles = self.workspace(cx, |ws, _, cx| tab_titles(ws, cx))?;
        self.check(
            opened && titles == ["main.rs — dockerbox"],
            &format!("main.rs opens in a tab named for dockerbox ({titles:?})"),
        );
        let text = cx.update(|window, cx| editor_text(window, cx))?;
        self.check(
            text.as_deref() == Some(original),
            "its contents match ssh cat",
        );
        self.shoot(cx, "rf-open");

        // Edit and save.
        self.type_text(cx, "// rf edit\n");
        Self::settle(cx).await;
        self.dispatch(cx, Box::new(crate::editor::Save));
        let edited = format!("// rf edit\n{original}");
        let saved = self
            .until_ssh(cx, &format!("cat {}", main_rs.display()), |out| {
                out == edited
            })
            .await;
        let clean = self
            .until(cx, Duration::from_secs(5), |_, window, cx| {
                !active_dirty(window, cx)
            })
            .await;
        self.check(saved && clean, "saving writes the edit (ssh cat shows it)");
        self.shoot(cx, "rf-saved");

        // A change on the machine reloads the clean buffer within the poll.
        ssh(
            cx,
            &format!("echo '// changed over ssh' >> {}", main_rs.display()),
            None,
        )
        .await;
        let theirs = format!("{edited}// changed over ssh\n");
        let reloaded = self
            .until(cx, Duration::from_secs(10), |_, window, cx| {
                editor_text(window, cx).as_deref() == Some(theirs.as_str())
                    && !active_dirty(window, cx)
            })
            .await;
        self.check(reloaded, "a change over ssh reloads the clean buffer");
        self.shoot(cx, "rf-reloaded");

        // Search.
        self.files(cx, |f, window, cx| f.set_query("hello", window, cx))?;
        let found = self
            .until(cx, Duration::from_secs(10), |_, _, cx| {
                files(cx)
                    .and_then(|f| f.search_results().cloned())
                    .is_some_and(|r| r.contents.iter().any(|hit| hit.path == main_rs))
            })
            .await;
        self.check(found, "searching hello finds src/main.rs");
        self.shoot(cx, "rf-search");
        self.files(cx, |f, window, cx| f.set_query("", window, cx))?;

        // Changes.
        self.dispatch(cx, Box::new(OpenChanges));
        let diffed = self
            .until(cx, Duration::from_secs(10), |_, window, cx| {
                crate::workspace::get(window, cx)
                    .and_then(|ws| ws.read(cx).changes().cloned())
                    .is_some_and(|changes| {
                        let changes = changes.read(cx);
                        changes
                            .patch()
                            .is_some_and(|p| p.contains("src/main.rs") && p.contains("+// rf edit"))
                            && changes.file_count(cx) >= 2
                    })
            })
            .await;
        self.check(diffed, "the Changes tab shows the diff");
        Self::sleep(cx, 500).await;
        self.shoot(cx, "rf-changes");

        // Create, rename, delete.
        let scratch = api_root.join(SCRATCH);
        let renamed = api_root.join(RENAMED);
        self.files(cx, |f, window, cx| {
            f.create_file(api_root.clone(), SCRATCH, window, cx)
        })?;
        let created = self
            .until_ssh(
                cx,
                &format!("test -f {} && echo yes", scratch.display()),
                |o| o == "yes\n",
            )
            .await;
        let in_tree = self.until_listed(cx, &api_root, SCRATCH, true).await;
        self.check(created && in_tree, "New File creates it on dockerbox");
        self.shoot(cx, "rf-created");
        self.files(cx, |f, window, cx| {
            f.rename_to(&scratch, RENAMED, window, cx)
        })?;
        let moved = self
            .until_ssh(
                cx,
                &format!(
                    "test -f {} && test ! -e {} && echo yes",
                    renamed.display(),
                    scratch.display()
                ),
                |o| o == "yes\n",
            )
            .await;
        let followed = self
            .until(cx, Duration::from_secs(5), |_, window, cx| {
                crate::workspace::get(window, cx).is_some_and(|ws| {
                    tab_titles(ws.read(cx), cx)
                        .iter()
                        .any(|t| t == &format!("{RENAMED} — dockerbox"))
                })
            })
            .await;
        self.check(
            moved && followed,
            "Rename renames it on dockerbox and its tab follows",
        );
        self.shoot(cx, "rf-renamed");
        self.files(cx, |f, window, cx| f.ask_to_remove(&renamed, window, cx))?;
        let asked = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(asked, "Delete asks first (permanent on dockerbox)");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "rf-delete-confirm");
        self.key(cx, "enter");
        let deleted = self
            .until_ssh(
                cx,
                &format!("test ! -e {} && echo yes", renamed.display()),
                |o| o == "yes\n",
            )
            .await;
        let gone = self.until_listed(cx, &api_root, RENAMED, false).await;
        let closed = self
            .until(cx, Duration::from_secs(5), |_, window, cx| {
                crate::workspace::get(window, cx)
                    .is_some_and(|ws| tab_titles(ws.read(cx), cx) == ["main.rs — dockerbox"])
            })
            .await;
        self.check(
            deleted && gone && closed,
            "confirming deletes it on dockerbox and closes its tab",
        );
        self.shoot(cx, "rf-deleted");

        // Offline: the tree says so, the editor keeps its text and refuses to save.
        self.workspace(cx, |ws, window, cx| ws.select_index(1, window, cx))?;
        docker(cx, &["stop", "-t", "1", CONTAINER]).await;
        let offline = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                remote(area, cx).is_some_and(|m| matches!(m.status(), Status::Offline { .. }))
            })
            .await;
        self.check(offline, "docker stop: dockerbox goes offline");
        self.type_text(cx, "x");
        Self::settle(cx).await;
        let mine = cx
            .update(|window, cx| editor_text(window, cx))?
            .unwrap_or_default();
        self.dispatch(cx, Box::new(crate::editor::Save));
        Self::sleep(cx, 800).await;
        let kept = cx.update(|window, cx| {
            mine != theirs
                && editor_text(window, cx).as_deref() == Some(mine.as_str())
                && active_dirty(window, cx)
        })?;
        self.check(kept, "offline: the save is refused and the edit kept");
        self.shoot(cx, "rf-offline");

        // Back: the tree lists again and the save goes through.
        docker(cx, &["start", CONTAINER]).await;
        self.bring_back(cx, &state, id).await;
        let relisted = self
            .until(cx, Duration::from_secs(20), |_, _, cx| {
                files(cx).is_some_and(|f| f.listed(&api_root).is_some() && f.root_error().is_none())
            })
            .await;
        self.check(relisted, "docker start: the tree recovers");
        self.dispatch(cx, Box::new(crate::editor::Save));
        let resaved = self
            .until_ssh(cx, &format!("cat {}", main_rs.display()), |out| out == mine)
            .await;
        self.check(resaved, "saving again after reconnect writes it");
        self.shoot(cx, "rf-recovered");

        // One path on Local and on dockerbox: two tabs.
        let made_dir = docker_root(cx, &format!("mkdir -p {SHARED} && chmod 1777 {SHARED}")).await;
        std::fs::write(shared, "on this Mac\n")?;
        ssh(
            cx,
            &format!("cat > {}", shared.display()),
            Some("on dockerbox\n"),
        )
        .await;
        self.check(made_dir, &format!("{SHARED} exists in the container"));
        for machine in [MachineId::Local, id.clone()] {
            self.dispatch(
                cx,
                Box::new(OpenFile {
                    path: shared.to_path_buf(),
                    machine,
                }),
            );
            Self::sleep(cx, 500).await;
        }
        let name = shared
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let both = self
            .until(cx, Duration::from_secs(5), |_, window, cx| {
                crate::workspace::get(window, cx).is_some_and(|ws| {
                    let titles = tab_titles(ws.read(cx), cx);
                    titles.contains(&name) && titles.contains(&format!("{name} — dockerbox"))
                })
            })
            .await;
        let titles = self.workspace(cx, |ws, _, cx| tab_titles(ws, cx))?;
        self.check(
            both,
            &format!("the same path on Local and dockerbox is two tabs ({titles:?})"),
        );
        self.shoot(cx, "rf-two-machines");
        Ok(())
    }

    /// Waits for dockerbox to reconnect, starting its herdr server if the
    /// container came back with sshd only.
    async fn bring_back(&mut self, cx: &mut Cx, state: &Entity<AppState>, id: &MachineId) {
        let reachable = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| {
                    matches!(m.status(), Status::Connected | Status::NeedsSetup(_))
                })
            })
            .await;
        self.check(reachable, "docker start: dockerbox is reachable again");
        let needs_server = self
            .read(cx, |area, cx| {
                remote(area, cx).is_some_and(|m| matches!(m.status(), Status::NeedsSetup(_)))
            })
            .unwrap_or(false);
        if needs_server {
            docker(
                cx,
                &[
                    "exec",
                    "-d",
                    "-u",
                    "lyle",
                    "-e",
                    "HOME=/home/lyle",
                    "-w",
                    "/home/lyle",
                    CONTAINER,
                    "/home/lyle/.local/bin/herdr",
                    "server",
                ],
            )
            .await;
            Self::sleep(cx, 1000).await;
            cx.update(|_, cx| state.update(cx, |state, cx| state.reconnect_machine(id, cx)))
                .ok();
        }
        let back = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| m.status().is_usable())
            })
            .await;
        self.check(back, "dockerbox reconnects");
    }

    /// Types `cd <dir>` into the selected machine's focused pane.
    async fn cd_focused_pane(&self, cx: &mut Cx, state: &Entity<AppState>, dir: &str) {
        let Ok(Some(pane)) = cx.update(|_, cx| state.read(cx).focused_pane(cx).map(PaneId::from))
        else {
            return;
        };
        let text = format!("cd {dir}\r");
        let Ok(task) = cx.update(|_, cx| {
            let state = state.read(cx);
            state.call(state.selected(), cx, move |client| {
                client.send::<method::PaneSendInput>(&protocol::PaneSendInputParams {
                    pane_id: pane,
                    text,
                    keys: Vec::new(),
                })
            })
        }) else {
            return;
        };
        let _ = task.await;
    }

    fn files<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut FileTree, &mut Window, &mut Context<FileTree>) -> R,
    ) -> anyhow::Result<R> {
        cx.update(|window, cx| {
            let files = sidebar::files(cx).ok_or_else(|| anyhow::anyhow!("no files panel"))?;
            Ok(files.update(cx, |files, cx| f(files, window, cx)))
        })?
    }

    /// Waits until `name` is (or isn't) listed in `dir`.
    async fn until_listed(&self, cx: &mut Cx, dir: &Path, name: &str, listed: bool) -> bool {
        self.until(cx, Duration::from_secs(10), |_, _, cx| {
            files(cx)
                .and_then(|f| f.listed(dir))
                .is_some_and(|names| names.iter().any(|n| n == name) == listed)
        })
        .await
    }

    /// Runs `command` over ssh until its output passes `test`.
    async fn until_ssh(&self, cx: &mut Cx, command: &str, test: impl Fn(&str) -> bool) -> bool {
        for _ in 0..20 {
            if ssh(cx, command, None).await.is_some_and(|out| test(&out)) {
                return true;
            }
            Self::sleep(cx, 500).await;
        }
        false
    }
}

fn files(cx: &App) -> Option<&FileTree> {
    let files = sidebar::files(cx)?;
    Some(files.read(cx))
}

fn editor_text(window: &Window, cx: &App) -> Option<String> {
    let workspace = crate::workspace::get(window, cx)?;
    workspace.read(cx).editor().read(cx).active_text(cx)
}

fn active_dirty(window: &Window, cx: &App) -> bool {
    let Some(workspace) = crate::workspace::get(window, cx) else {
        return false;
    };
    let editor = workspace.read(cx).editor().read(cx);
    editor
        .active_index()
        .and_then(|index| editor.tabs(cx).get(index).map(|tab| tab.dirty))
        .unwrap_or(false)
}

/// Runs `command` on dockerbox over plain ssh (not the app's link), feeding
/// it `input`; its stdout if it succeeded.
async fn ssh(cx: &mut Cx, command: &str, input: Option<&str>) -> Option<String> {
    let command = command.to_owned();
    let input = input.map(str::to_owned);
    cx.background_executor()
        .spawn(async move {
            use std::io::Write as _;
            use std::process::{Command, Stdio};
            let mut child = Command::new("ssh")
                .args(SSH)
                .arg(&command)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .ok()?;
            let mut stdin = child.stdin.take()?;
            if let Some(input) = input {
                stdin.write_all(input.as_bytes()).ok()?;
            }
            drop(stdin);
            let output = child.wait_with_output().ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        })
        .await
}

/// Runs `command` as root in the container.
async fn docker_root(cx: &mut Cx, command: &str) -> bool {
    let command = command.to_owned();
    cx.background_executor()
        .spawn(async move {
            std::process::Command::new("docker")
                .args(["exec", "-u", "0", CONTAINER, "sh", "-c", &command])
                .status()
                .is_ok_and(|s| s.success())
        })
        .await
}
