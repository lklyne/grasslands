//! `RT_SCRIPT=machine-ui`: the remote-machine UI pieces in the running app,
//! with fixture data (no machine connects). A gallery layer over the window
//! shows group headers in every state, the offline screens and the remote
//! file panel; then a header's menu; then the Add Machine dialog with fixture
//! `~/.ssh/config` hosts, typed into and confirmed by key presses. Each step
//! is screenshotted to `target/script-shots/`.
//!
//! ```sh
//! RT_SCRIPT=machine-ui scripts/dev-run cargo run -p app
//! ```

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::{Cx, Run};
use crate::chrome::Callback;
use crate::machines::{
    self, MachineActions, MachineState, SshHost, machine_header, offline_screen, remote_files_empty,
};
use crate::overlay::{self, Capture};

const FIXTURE_CONFIG: &str = "\
Host gpu-lab
    HostName 10.0.4.21
    User lyle
Host gpu-box-2
    HostName gpu2.internal
    User lyle
Host workbox
    HostName workbox.local
Host dockerbox
    HostName 127.0.0.1
    Port 2222
Host *
    User lyle
";

const SIDEBAR: Pixels = px(262.);

fn log(what: &'static str) -> Callback {
    Rc::new(move |_, _| eprintln!("script: {what}"))
}

struct Gallery;

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let row = |label: &'static str, dim: bool| {
            div()
                .h(px(20.))
                .pl(px(20.))
                .text_sm()
                .text_color(theme.foreground.opacity(if dim { 0.4 } else { 0.78 }))
                .child(label)
        };
        let header = |id: &'static str, label: &'static str, state: MachineState| {
            machine_header(id, label, state)
                .on_toggle(move |_, _| eprintln!("script: toggle {id}"))
                .on_menu(move |_, _, _| eprintln!("script: menu {id}"))
        };
        let sidebar = v_flex()
            .w(SIDEBAR)
            .h_full()
            .flex_shrink_0()
            .p_1()
            .border_r_1()
            .border_color(theme.border)
            .child(machine_header("local", "This Mac", MachineState::Connected))
            .child(row("home", false))
            .child(row("grasslands", false))
            .child(header("workbox", "workbox", MachineState::Connected))
            .child(row("api", false))
            .child(row("infra", false))
            .child(header("connecting", "build-01", MachineState::Connecting))
            .child(header("gpu", "gpu-box", MachineState::Offline))
            .child(row("train", true))
            .child(header("signin", "lab-mac", MachineState::NeedsSignIn))
            .child(header("setup", "fresh-vm", MachineState::NeedsSetup).collapsed(true))
            .child(header("disabled", "old-server", MachineState::Disabled).collapsed(true));
        let screen = |state: MachineState, detail: Option<&str>| {
            div()
                .flex_1()
                .h_full()
                .border_1()
                .border_color(theme.border)
                .child(offline_screen(
                    "gpu-box",
                    state,
                    detail,
                    log("reconnect"),
                    Some(log("fix")),
                    cx,
                ))
        };
        let screens = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_1()
            .p_1()
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .gap_1()
                    .child(screen(MachineState::Offline, Some("Connection timed out")))
                    .child(screen(MachineState::NeedsSignIn, None)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .gap_1()
                    .child(screen(
                        MachineState::NeedsSetup,
                        Some("herdr isn't installed there"),
                    ))
                    .child(screen(MachineState::Connecting, None)),
            );
        let files = div()
            .w(SIDEBAR)
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(theme.border)
            .child(remote_files_empty("workbox", cx));
        h_flex()
            .id("machine-ui-gallery")
            .occlude()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(sidebar)
            .child(screens)
            .child(files)
    }
}

fn actions() -> MachineActions {
    MachineActions {
        on_reconnect: log("reconnect"),
        on_fix: log("fix"),
        on_rename: Rc::new(|name, _, _| eprintln!("script: rename to {name}")),
        on_set_enabled: Rc::new(|on, _, _| eprintln!("script: set enabled {on}")),
        on_remove: log("remove"),
    }
}

impl Run {
    pub(super) async fn machine_ui(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: machine-ui");
        let hosts = machines::ssh_config::from_text(FIXTURE_CONFIG, Path::new("/nonexistent"));
        self.check(
            hosts.len() == 4 && hosts[3].destination() == "lyle@127.0.0.1:2222",
            "fixture hosts parse",
        );

        // The gallery, over everything.
        let layer = cx.update(|window, cx| {
            let overlay = overlay::get_or_open(window, cx).ok()?;
            let view = cx.new(|_| Gallery);
            let id = overlay.update(cx, |o, cx| o.push_layer(view, Capture::Modal, cx));
            Some((overlay, id))
        })?;
        let Some((overlay, layer)) = layer else {
            anyhow::bail!("no overlay");
        };
        Self::settle(cx).await;
        Self::sleep(cx, 300).await;
        self.shoot_with_overlay(cx, "machine-gallery");

        // The ··· menu of a machine that needs setup, at the header's row.
        cx.update(|window, cx| {
            let entries = machines::machine_menu("fresh-vm", MachineState::NeedsSetup, actions());
            machines::open_machine_menu(entries, point(SIDEBAR - px(10.), px(266.)), window, cx);
        })?;
        let menu = self
            .until(cx, std::time::Duration::from_secs(3), |_, window, cx| {
                crate::chrome::get(window, cx)
                    .is_some_and(|chrome| chrome.read(cx).menu_open().is_some())
            })
            .await;
        self.check(menu, "the machine menu opens");
        Self::sleep(cx, 200).await;
        self.shoot_with_overlay(cx, "machine-menu-setup");
        self.key(cx, "escape");
        Self::settle(cx).await;
        cx.update(|window, cx| {
            let entries = machines::machine_menu("old-server", MachineState::Disabled, actions());
            machines::open_machine_menu(entries, point(SIDEBAR - px(10.), px(294.)), window, cx);
        })?;
        Self::settle(cx).await;
        Self::sleep(cx, 200).await;
        self.shoot_with_overlay(cx, "machine-menu-disabled");
        self.key(cx, "escape");
        Self::settle(cx).await;
        cx.update(|_, cx| overlay.update(cx, |o, cx| o.remove_layer(layer, cx)))?;
        Self::settle(cx).await;

        // Add Machine: "gpu" narrows to the two gpu hosts, down picks the
        // second, Enter connects to it.
        let connected: Rc<RefCell<Vec<String>>> = Rc::default();
        self.open_add_machine(cx, &hosts, &connected)?;
        let open = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(open, "Add Machine opens in the overlay");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "add-machine-empty");
        self.type_keys(cx, "gpu");
        Self::settle(cx).await;
        Self::sleep(cx, 200).await;
        self.shoot_with_overlay(cx, "add-machine-gpu");
        self.key(cx, "down");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "add-machine-gpu-down");
        self.key(cx, "enter");
        let closed = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        Self::settle(cx).await;
        self.check(
            closed && connected.borrow().as_slice() == ["gpu-box-2"],
            &format!(
                "Enter connects to the highlighted host (got {:?})",
                connected.borrow()
            ),
        );

        // Typed text that matches nothing is the host.
        connected.borrow_mut().clear();
        self.open_add_machine(cx, &hosts, &connected)?;
        self.until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.type_keys(cx, "me@new-box");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "add-machine-typed");
        self.key(cx, "enter");
        self.until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        Self::settle(cx).await;
        self.check(
            connected.borrow().as_slice() == ["me@new-box"],
            &format!(
                "Enter connects to the typed host (got {:?})",
                connected.borrow()
            ),
        );

        // Escape clears the field, then closes without connecting.
        connected.borrow_mut().clear();
        self.open_add_machine(cx, &hosts, &connected)?;
        self.until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.type_keys(cx, "gpu");
        Self::settle(cx).await;
        self.key(cx, "escape");
        Self::settle(cx).await;
        self.key(cx, "escape");
        let closed = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(
            closed && connected.borrow().is_empty(),
            "Escape closes Add Machine without connecting",
        );

        // With no ssh config hosts.
        self.open_add_machine(cx, &[], &connected)?;
        self.until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "add-machine-no-hosts");
        self.key(cx, "escape");
        self.until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        Ok(())
    }

    fn open_add_machine(
        &self,
        cx: &mut Cx,
        hosts: &[SshHost],
        connected: &Rc<RefCell<Vec<String>>>,
    ) -> anyhow::Result<()> {
        let (hosts, connected) = (hosts.to_vec(), connected.clone());
        cx.update(|window, cx| {
            machines::open_add_machine(
                hosts,
                move |host, _, _| {
                    eprintln!("script: connect to {host}");
                    connected.borrow_mut().push(host);
                },
                window,
                cx,
            );
        })
    }

    fn type_keys(&self, cx: &mut Cx, text: &str) {
        cx.update(|window, cx| {
            for keystroke in overlay::keystrokes(text) {
                overlay::dispatch_keystroke(keystroke, window, cx);
            }
        })
        .ok();
    }
}
