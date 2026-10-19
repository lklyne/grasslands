//! `RT_SCRIPT=settings`: the Settings window. Opens it with cmd-, (a GPUI
//! keystroke in the main window), checks a second cmd-, reuses it, picks
//! themes through the same path its dropdowns take, and checks the app theme
//! and the terminals follow. Screenshots the Settings and main windows. The
//! saved settings file is not written during script runs.
//!
//! ```sh
//! RT_SCRIPT=settings scripts/dev-run cargo run -p app
//! ```

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::*;

use super::{Cx, Run, capture};
use crate::overlay;
use crate::settings;
use crate::terminal_area::hooks::Hooks;
use crate::theme::{self, Appearance};

impl Run {
    pub(super) async fn settings(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: settings");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0 && area.terminals().len() > 0
            })
            .await;
        self.check(connected, "connected to herdr with a terminal");
        let original = cx.update(|_, cx| theme::settings(cx).clone())?;

        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-,").unwrap_or_default(), cx);
        })?;
        let opened = self
            .until(cx, Duration::from_secs(5), |_, _, cx| {
                settings::window(cx).is_some()
            })
            .await;
        self.check(opened, "cmd-, opens the Settings window");
        let Some(first) = cx.update(|_, cx| settings::window(cx))? else {
            anyhow::bail!("no Settings window");
        };
        Self::sleep(cx, 500).await;
        self.shoot_settings(cx, "settings-window", first);

        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-,").unwrap_or_default(), cx);
        })?;
        Self::settle(cx).await;
        let reused = cx.update(|_, cx| settings::window(cx))? == Some(first);
        self.check(reused, "cmd-, again brings the same window forward");

        let read_config = |area: &Hooks| {
            area.terminals()
                .iter()
                .next()
                .and_then(|(_, view)| std::fs::read_to_string(view.config_path()).ok())
        };
        let before = self.read(cx, |area, _| read_config(area))?;
        cx.update(|_, cx| {
            theme::update_settings(cx, |s| s.appearance = Appearance::Dark);
            settings::pick(ThemeMode::Dark, "Tokyo Night".into(), cx);
        })?;
        let tokyo = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                cx.theme().theme_name() == "Tokyo Night" && read_config(area) != before
            })
            .await;
        self.check(tokyo, "picking Tokyo Night themes the app and terminals");
        Self::sleep(cx, 500).await;
        self.shoot(cx, "tokyo-night-main");
        self.shoot_settings(cx, "tokyo-night-settings", first);

        cx.update(|_, cx| settings::pick(ThemeMode::Light, "Ayu Light".into(), cx))?;
        Self::settle(cx).await;
        let ayu = cx.update(|_, cx| {
            !cx.theme().is_dark()
                && cx.theme().theme_name() == "Ayu Light"
                && theme::settings(cx).appearance == Appearance::Light
        })?;
        self.check(
            ayu,
            "picking a light theme while fixed dark switches to light",
        );
        Self::sleep(cx, 500).await;
        self.shoot(cx, "ayu-light-main");

        cx.update(|_, cx| theme::update_settings(cx, |s| *s = original.clone()))?;
        let closed = first.update(cx, |_, window, _| window.remove_window());
        self.check(closed.is_ok(), "Settings window closes");
        Ok(())
    }

    /// Screenshot of the Settings window alone.
    fn shoot_settings(&mut self, cx: &mut Cx, name: &str, handle: AnyWindowHandle) {
        self.shot += 1;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join(format!("target/script-shots/{:02}-{name}.png", self.shot));
        let number = handle
            .update(cx, |_, window, _| {
                overlay::ns_window(window).map(|w| w.windowNumber())
            })
            .ok()
            .flatten();
        let Some(number) = number else {
            eprintln!("shot {name} skipped: no window");
            return;
        };
        let mut command = std::process::Command::new("swift");
        command
            .arg(root.join("scripts/capture-windows.swift"))
            .arg(&path)
            .arg(number.to_string());
        match capture(command, Duration::from_secs(20)) {
            Ok(()) if path.exists() => eprintln!("shot {}", path.display()),
            Ok(()) => eprintln!("shot {name} skipped: no file written"),
            Err(error) => eprintln!("shot {name} skipped: {error}"),
        }
    }
}
