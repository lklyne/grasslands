//! The two dev-session panes the spike attaches to. Found through the herdr
//! CLI so the spike stays independent of crates/herdr-client.

use std::process::Command;

use anyhow::{Context as _, bail};
use serde_json::Value;

pub struct Pane {
    pub pane_id: String,
    pub terminal_id: String,
}

fn herdr_bin() -> String {
    std::env::var("HERDR_BIN").unwrap_or_else(|_| "herdr".to_owned())
}

fn herdr(args: &[&str]) -> anyhow::Result<Value> {
    serde_json::from_slice(&herdr_raw(args)?).context("parse herdr output")
}

fn herdr_raw(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut command = Command::new(herdr_bin());
    if let Ok(session) = std::env::var("HERDR_SESSION") {
        command.args(["--session", &session]);
    }
    let output = command.args(args).output().context("run herdr")?;
    if !output.status.success() {
        bail!(
            "herdr {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

/// The command a Ghostty surface runs to take over `terminal_id`.
pub fn attach_command(terminal_id: &str) -> String {
    match std::env::var("HERDR_SESSION") {
        Ok(session) => format!(
            "{} --session {session} terminal attach {terminal_id} --takeover",
            herdr_bin()
        ),
        Err(_) => format!("{} terminal attach {terminal_id} --takeover", herdr_bin()),
    }
}

/// The first two panes of the workspace labelled `spike`.
pub fn spike_panes() -> anyhow::Result<Vec<Pane>> {
    if std::env::var("HERDR_SESSION").is_err() {
        bail!(
            "HERDR_SESSION is unset; run through scripts/dev-run so the live session is never touched"
        );
    }
    let workspaces = herdr(&["workspace", "list"])?;
    let workspace_id = workspaces["result"]["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|workspace| workspace["label"] == "spike")
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .context("no `spike` workspace in the dev session; create one and split it")?
        .to_owned();
    let panes = herdr(&["pane", "list"])?;
    let panes: Vec<Pane> = panes["result"]["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pane| pane["workspace_id"] == workspace_id.as_str())
        .filter_map(|pane| {
            Some(Pane {
                pane_id: pane["pane_id"].as_str()?.to_owned(),
                terminal_id: pane["terminal_id"].as_str()?.to_owned(),
            })
        })
        .take(2)
        .collect();
    if panes.len() < 2 {
        bail!("the `spike` workspace needs two panes; split it with scripts/herdr-dev pane split");
    }
    Ok(panes)
}

/// The visible text of the `index`th spike pane, read back through herdr.
pub fn read_pane(index: usize) -> String {
    let Ok(panes) = spike_panes() else {
        return String::new();
    };
    let Some(pane) = panes.get(index) else {
        return String::new();
    };
    herdr_raw(&["pane", "read", &pane.pane_id])
        .map(|text| String::from_utf8_lossy(&text).into_owned())
        .unwrap_or_else(|error| format!("{error:#}"))
}
