//! `RT_SCRIPT=herdr-prefs`: herdr's config.toml through the app. Writes
//! preferences the way the Settings window's herdr page does, checks the
//! file changed with every comment kept, that `herdr config check` accepts
//! it and Local's herdr reloaded it cleanly, then edits the file from
//! outside and checks the app follows. Restores the file at the end.
//!
//! It writes the config, so point `HERDR_CONFIG_PATH` at a scratch copy
//! under the dev root, and start the dev server with the same variable so
//! `server.reload_config` reads that copy too:
//!
//! ```sh
//! mkdir -p ~/.local/share/grasslands-herdr/prefs-test
//! ~/.local/herdr-0.9.5/herdr --default-config > ~/.local/share/grasslands-herdr/prefs-test/config.toml
//! env HERDR_CONFIG_PATH=$HOME/.local/share/grasslands-herdr/prefs-test/config.toml \
//!   RT_SCRIPT=herdr-prefs scripts/dev-run cargo run -p app
//! ```

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;

use super::{Cx, Run};
use crate::herdr::config::{
    self, AgentPanelSort, AgentSound, CONFIG_PATH_ENV, Edit, ToastDelivery,
};
use crate::herdr_prefs;

/// The edits the script makes, as the Settings page would.
fn edits() -> Vec<Edit> {
    vec![
        Edit::ConfirmClose(false),
        Edit::ToastDelivery(ToastDelivery::Herdr),
        Edit::ToastDelay(0),
        Edit::AgentSound {
            agent: "claude".into(),
            sound: AgentSound::Off,
        },
        Edit::AgentPanelSort(AgentPanelSort::Priority),
    ]
}

fn comments(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .collect()
}

impl Run {
    pub(super) async fn herdr_prefs(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: herdr-prefs");
        anyhow::ensure!(
            std::env::var_os(CONFIG_PATH_ENV).is_some(),
            "set {CONFIG_PATH_ENV} to a scratch config (see the module docs)"
        );
        let path: PathBuf = cx
            .update(|_, cx| herdr_prefs::path(cx).map(PathBuf::from))?
            .context("no herdr config path")?;
        let original = std::fs::read_to_string(&path).unwrap_or_default();
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");

        let mut expected = cx.update(|_, cx| herdr_prefs::get(cx).clone())?;
        for edit in edits() {
            edit.apply_to(&mut expected);
        }
        cx.update(|_, cx| {
            for edit in edits() {
                herdr_prefs::set(edit, cx);
            }
        })?;
        let in_app = cx.update(|_, cx| herdr_prefs::get(cx) == &expected)?;
        self.check(in_app, "edits apply in the app at once");
        let written = self
            .until(cx, Duration::from_secs(10), |_, _, _| {
                config::load(&path).prefs == expected
            })
            .await;
        self.check(written, "edits land in herdr's config.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.check(
            comments(&text) == comments(&original),
            "every comment in config.toml is kept",
        );

        let bin = self
            .read(cx, |area, cx| {
                area.state()
                    .read(cx)
                    .herdr_config
                    .as_ref()
                    .map(|c| c.bin.clone())
            })?
            .context("no herdr binary")?;
        let check_path = path.clone();
        let report = cx
            .background_executor()
            .spawn(async move { config::check(&bin, &check_path) })
            .await?;
        self.check(
            report.ok,
            &format!("herdr config check passes: {}", report.details),
        );
        let reloaded = self
            .until(cx, Duration::from_secs(10), |_, _, cx| {
                herdr_prefs::problems(cx).is_empty()
            })
            .await;
        let problems = cx.update(|_, cx| herdr_prefs::problems(cx))?;
        self.check(
            reloaded,
            &format!("herdr reloads it with no diagnostics: {problems:?}"),
        );

        // An edit from outside the app (an editor, herdr itself).
        std::fs::write(
            &path,
            text.replace("confirm_close = false", "confirm_close = true"),
        )?;
        let followed = self
            .until(cx, Duration::from_secs(5), |_, _, cx| {
                herdr_prefs::get(cx).confirm_close
            })
            .await;
        self.check(followed, "an outside edit reaches the app");

        std::fs::write(&path, &original)?;
        let restored = self
            .until(cx, Duration::from_secs(5), |_, _, cx| {
                herdr_prefs::get(cx) == &config::parse(&original, path.parent()).prefs
            })
            .await;
        self.check(restored, "restoring the file restores the app's prefs");
        cx.update(|_, cx| herdr_prefs::reload_server(cx))?;
        Ok(())
    }
}
