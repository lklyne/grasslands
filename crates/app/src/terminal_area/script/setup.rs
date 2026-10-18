//! `RT_SCRIPT=setup`: the herdr setup screen. Waits for the launch check,
//! checks it found what `RT_SETUP_EXPECT` says (`ready`, `missing`,
//! `outdated`, `server-outdated`, `unreachable`) and screenshots the screen.
//! With `RT_SETUP_FIX=1` it then runs the offered fix the way the button
//! does and checks the app connects; with `RT_SETUP_FIX=fails` it checks a
//! failing fix is reported.
//!
//! Point it at a dev session only; the fix for `server-outdated` stops it:
//!
//! ```sh
//! HERDR_BIN=/nonexistent/herdr RT_SETUP_EXPECT=missing RT_SCRIPT=setup scripts/dev-run cargo run -p app
//! ```

use std::time::Duration;

use gpui_kit::component::WindowExt as _;

use super::{Cx, Run};
use crate::herdr::{Check, ConnectionStatus, Fix};
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
            Stage::Needed(Check::Ready) | Stage::Checking | Stage::Fixing(_) => "unsettled",
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
                crate::setup::render(&area.state, cx).is_some() == (expect != "ready")
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
        if fix == Fix::StopServer {
            // The button's path: the confirmation, then Enter on it.
            cx.update(|window, cx| crate::setup::confirm_restart(&state, window, cx))?;
            let asked = self
                .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
                .await;
            self.check(asked, "Restart herdr asks first");
            Self::settle(cx).await;
            self.shoot_with_overlay(cx, "setup-restart-confirm");
            self.key(cx, "enter");
        } else {
            cx.update(|_, cx| state.update(cx, |state, cx| state.run_fix(fix, cx)))?;
        }
        let showing = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let app = area.state.read(cx);
                !matches!(app.setup.stage, Stage::Fixing(_))
                    || app
                        .setup
                        .run
                        .as_ref()
                        .is_some_and(|run| !run.output.is_empty())
            })
            .await;
        self.check(showing, "the fix's output shows while it runs");
        self.shoot(cx, "setup-fixing");
        if std::env::var("RT_SETUP_FIX").as_deref() == Ok("fails") {
            let failed = self
                .until(cx, Duration::from_secs(60), |area, _, cx| {
                    let app = area.state.read(cx);
                    matches!(app.setup.stage, Stage::Needed(_))
                        && app
                            .setup
                            .run
                            .as_ref()
                            .is_some_and(|run| run.failure.is_some())
                })
                .await;
            self.check(failed, "a failed fix says so and offers the fix again");
            self.shoot(cx, "setup-fix-failed");
            return Ok(());
        }
        let connected = self
            .until(cx, Duration::from_secs(120), |area, _, cx| {
                let app = area.state.read(cx);
                app.setup.is_ready() && app.snapshot.generation > 0
            })
            .await;
        let run = cx.update(|_, cx| state.read(cx).setup.run.clone())?;
        if let Some(run) = &run {
            eprintln!(
                "script: `{}` printed {} lines",
                run.command,
                run.output.len()
            );
        }
        self.check(
            connected && run.is_some_and(|run| run.failure.is_none()),
            "the fix ran and the app connected",
        );
        self.shoot(cx, "setup-fixed");
        Ok(())
    }
}
