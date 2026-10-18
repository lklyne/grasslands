//! `RT_SCRIPT=setup`: the herdr setup screen. Waits for the launch check,
//! checks it found what `RT_SETUP_EXPECT` says (`ready`, `missing`,
//! `outdated`, `server-outdated`, `unreachable`) and screenshots the screen.
//! With `RT_SETUP_FIX=1` it then runs the offered fix the way the button
//! does, in the setup terminal, and checks the screen offers Finish Setup
//! and the app connects; with `RT_SETUP_FIX=fails` it checks a failing fix
//! leaves the screen offering the fix again.
//!
//! Point it at a dev session only; the fix for `server-outdated` stops it.
//! `scripts/dev-run` sets `HERDR_BIN`, so override it after, with `env`. A
//! stand-in `brew` first on `PATH` keeps a fix from installing anything:
//!
//! ```sh
//! RT_SETUP_EXPECT=missing RT_SCRIPT=setup scripts/dev-run env HERDR_BIN=/nonexistent/herdr cargo run -p app
//! ```
//!
//! `unreachable` needs a session with no server (dev-run starts one for its
//! own): add `HERDR_SESSION=setup-unreachable` and that session's socket
//! under `~/.local/share/grasslands-herdr/herdr/sessions/`.

use std::time::Duration;

use super::{Cx, Run};
use crate::herdr::{Check, ConnectionStatus};
use crate::setup::Stage;

impl Run {
    pub(super) async fn setup(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        let expect = std::env::var("RT_SETUP_EXPECT").unwrap_or_else(|_| "ready".into());
        eprintln!("script: setup, expecting {expect}");
        let state = self.read(cx, |area, _| area.state.clone())?;
        let settled = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                let app = area.state.read(cx);
                match expect.as_str() {
                    "unreachable" => {
                        matches!(app.connection, ConnectionStatus::Reconnecting { .. })
                    }
                    _ => !matches!(app.setup.stage, Stage::Checking),
                }
            })
            .await;
        let stage = cx.update(|_, cx| state.read(cx).setup.stage.clone())?;
        eprintln!("script: stage {stage:?}");
        let found = match &stage {
            Stage::Ready => "ready",
            Stage::Needed(Check::Missing) => "missing",
            Stage::Needed(Check::Outdated(_)) => "outdated",
            Stage::Needed(Check::ServerOutdated { .. }) => "server-outdated",
            Stage::Needed(Check::Broken { .. }) => "broken",
            Stage::Needed(Check::Ready) | Stage::Checking | Stage::Fixing(_) | Stage::Done => {
                "unsettled"
            }
        };
        let found = if expect == "unreachable" && settled && found == "ready" {
            "unreachable"
        } else {
            found
        };
        self.check(
            found == expect,
            &format!("the check found {expect} (got {found})"),
        );
        let drawn = self
            .until(cx, Duration::from_secs(2), |area, _, cx| {
                crate::setup::render(&area.state, None, cx).is_some() == (expect != "ready")
            })
            .await;
        self.check(
            drawn,
            "the terminal area draws the setup screen when it should",
        );
        self.shoot(cx, &format!("setup-{expect}"));

        if std::env::var("RT_SETUP_FIX").is_err() {
            return Ok(());
        }
        let Stage::Needed(check) = stage else {
            return Ok(());
        };
        let Some(fix) = check.fix() else {
            self.fail("no fix offered");
            return Ok(());
        };
        eprintln!("script: running {fix:?}");
        cx.update(|_, cx| state.update(cx, |state, cx| state.run_fix(fix, cx)))?;
        let showing = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.setup_terminal.is_some()
            })
            .await;
        self.check(showing, "the fix runs in a terminal beside the screen");
        Self::settle(cx).await;
        self.shoot(cx, "setup-fixing");
        if std::env::var("RT_SETUP_FIX").as_deref() == Ok("fails") {
            let failed = self
                .until(cx, Duration::from_secs(60), |area, _, cx| {
                    matches!(area.state.read(cx).setup.stage, Stage::Needed(_))
                })
                .await;
            self.check(failed, "a failed fix offers the fix again");
            self.shoot(cx, "setup-fix-failed");
            return Ok(());
        }
        let done = self
            .until(cx, Duration::from_secs(120), |area, _, cx| {
                area.state.read(cx).setup.stage == Stage::Done
            })
            .await;
        self.check(done, "a fix that worked offers Finish Setup");
        self.shoot(cx, "setup-done");
        cx.update(|_, cx| state.update(cx, |state, cx| state.finish_setup(cx)))?;
        let connected = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                let app = area.state.read(cx);
                app.setup.is_ready() && app.snapshot.generation > 0 && area.setup_terminal.is_none()
            })
            .await;
        self.check(
            connected,
            "finishing closes the screen and the app connects",
        );
        self.shoot(cx, "setup-fixed");
        Ok(())
    }
}
