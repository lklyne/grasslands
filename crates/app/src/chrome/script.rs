//! Debug builds only: drive the chrome from `RT_CHROME_SCRIPT` so its flows
//! can be checked live without synthetic OS input. Steps are `;`-separated:
//!
//! - `wait <ms>`
//! - `menu pane|tab|workspace <id>`: open that context menu
//! - `run close-pane|close-tab|close-workspace|close-group <id>`: `Chrome::run`
//! - `palette`, `query <text>`: open the palette, type into it
//! - `key down|enter|escape`: dispatch `SelectDown`, `Confirm` or `Cancel` to
//!   the focused element (the open menu or dialog)
//! - `log <label>`: print what is open and herdr's panes and agents to stderr
//!
//! e.g. `RT_CHROME_SCRIPT='menu pane w1:p1; wait 1500; key escape; log after'`.

use std::time::Duration;

use gpui_kit::base::actions::{Cancel, Confirm, SelectDown};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use super::{Chrome, HerdrCommand, MenuTarget};
use crate::attention::{agent_name, status_label};

/// Pause between steps so each one renders (and can be screenshotted).
const STEP: Duration = Duration::from_millis(400);

pub(super) fn start(window: &mut Window, cx: &mut Context<Chrome>) {
    let Ok(script) = std::env::var("RT_CHROME_SCRIPT") else {
        return;
    };
    let steps: Vec<String> = script
        .split(';')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    cx.spawn_in(window, async move |this, cx| {
        // Let the first snapshot arrive.
        cx.background_executor().timer(Duration::from_secs(2)).await;
        for step in steps {
            eprintln!("[chrome-script] {step}");
            if let Some(ms) = step.strip_prefix("wait ") {
                let ms = ms.trim().parse().unwrap_or(0);
                cx.background_executor()
                    .timer(Duration::from_millis(ms))
                    .await;
                continue;
            }
            // Keys come from the window, like real input, not from inside the chrome.
            if let Some(key) = step.strip_prefix("key ") {
                let action: Box<dyn Action> = match key.trim() {
                    "down" => Box::new(SelectDown),
                    "enter" => Box::new(Confirm { secondary: false }),
                    "escape" => Box::new(Cancel),
                    other => {
                        eprintln!("[chrome-script] unknown key {other:?}");
                        continue;
                    }
                };
                if cx.update(|window, cx| window.dispatch_action(action, cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(STEP).await;
                continue;
            }
            let done = this.update_in(cx, |chrome, window, cx| {
                if let Err(error) = run_step(chrome, &step, window, cx) {
                    eprintln!("[chrome-script] {step}: {error}");
                }
            });
            if done.is_err() {
                return;
            }
            cx.background_executor().timer(STEP).await;
        }
        eprintln!("[chrome-script] end");
    })
    .detach();
}

fn run_step(
    chrome: &mut Chrome,
    step: &str,
    window: &mut Window,
    cx: &mut Context<Chrome>,
) -> Result<(), String> {
    let mut words = step.splitn(3, ' ');
    let verb = words.next().unwrap_or_default();
    let first = words.next().unwrap_or_default();
    let rest = words.next().unwrap_or_default();
    match verb {
        "menu" => {
            let id = rest.to_owned();
            let target = match first {
                "pane" => MenuTarget::Pane { pane_id: id },
                "tab" => MenuTarget::Tab { tab_id: id },
                "workspace" => MenuTarget::Workspace { workspace_id: id },
                other => return Err(format!("unknown menu target {other:?}")),
            };
            chrome.context_menu(target, point(px(320.), px(200.)), Vec::new(), window, cx);
        }
        "run" => {
            let id = rest.to_owned();
            let command = match first {
                "close-pane" => HerdrCommand::ClosePane { pane_id: id },
                "close-tab" => HerdrCommand::CloseTab { tab_id: id },
                "close-workspace" => HerdrCommand::CloseWorkspace {
                    workspace_id: id,
                    close_group: false,
                },
                "close-group" => HerdrCommand::CloseWorkspace {
                    workspace_id: id,
                    close_group: true,
                },
                other => return Err(format!("unknown command {other:?}")),
            };
            chrome.run(command, window, cx);
        }
        "palette" => chrome.open_command_palette(window, cx),
        "query" => {
            let query = step.strip_prefix("query ").unwrap_or_default().to_owned();
            let palette = chrome
                .palette
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .ok_or("no palette open")?;
            palette.update(cx, |palette, cx| palette.set_query(query, window, cx));
        }
        "log" => log(chrome, first, window, cx),
        other => return Err(format!("unknown step {other:?}")),
    }
    Ok(())
}

fn log(chrome: &Chrome, label: &str, window: &mut Window, cx: &mut Context<Chrome>) {
    let dialog = window.has_active_dialog(cx);
    let toasts = window.notifications(cx).len();
    let snapshot = &chrome.state.read(cx).snapshot;
    let panes: Vec<&str> = snapshot.panes.keys().map(String::as_str).collect();
    let agents: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| {
            format!(
                "{}={}:{}",
                a.pane_id,
                agent_name(a),
                status_label(a.agent_status)
            )
        })
        .collect();
    let tabs: Vec<String> = snapshot
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .map(|t| format!("{}={}", t.info.tab_id, t.info.label))
        .collect();
    let workspaces: Vec<String> = snapshot
        .workspaces
        .iter()
        .map(|w| format!("{}={}", w.info.workspace_id, w.info.label))
        .collect();
    eprintln!(
        "[chrome-script] log {label}: menu_open={} dialog={} toasts={} focused={:?} \
         workspaces={workspaces:?} tabs={tabs:?} panes={panes:?} agents={agents:?}",
        chrome.open_menu.is_some(),
        dialog,
        toasts,
        snapshot.focused_pane_id,
    );
}
