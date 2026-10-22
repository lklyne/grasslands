//! One Ghostty view per herdr terminal.
//!
//! Views are keyed by `terminal_id`, not pane id: a pane moved to another tab
//! or workspace keeps its terminal (and so its view and process), while a
//! herdr restart hands out new terminal ids and so gets fresh views. The set
//! is reconciled against every snapshot herdr-client delivers, which follows
//! `pane.created`, `pane.closed` and `pane.moved` within one re-read.
//!
//! Terminal ids are per herdr server, so views are keyed by machine too. Only
//! the selected machine's views are shown; another machine's stay attached
//! (in the background) until their panes close or the machine goes away.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use gpui_libghostty::{TerminalConfiguration, TerminalOptions};

use crate::herdr::{self, PaneId, PaneInfo, Snapshot, TerminalId};
use crate::state::{MachineId, On};

gpui_libghostty::bind_gpui!(gpui_kit);

/// The command terminals' name for their config file; herdr's terminal ids
/// all start with `term_`.
const COMMAND_TERMINAL: &str = "setup-terminal";

pub struct TerminalView {
    pub terminal: Entity<Terminal>,
    pub pane_id: PaneId,
    config_path: PathBuf,
    /// What this view was last told about its visibility.
    shown: bool,
}

impl TerminalView {
    /// The Ghostty config file this terminal was loaded from.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

/// How to start `herdr terminal attach`.
#[derive(Debug, Clone)]
pub struct Launch {
    pub bin: PathBuf,
    pub session: Option<String>,
}

impl Launch {
    pub fn from_config(config: &herdr::Config) -> Self {
        Self {
            bin: config.bin.clone(),
            session: config.session.clone(),
        }
    }

    /// Without `takeover`, herdr refuses a terminal another client has
    /// attached; with it, that client is detached instead.
    pub fn attach_command(&self, terminal_id: &TerminalId, takeover: bool) -> String {
        let bin = shell_quote(&self.bin.to_string_lossy());
        let session = self
            .session
            .as_deref()
            .map(|session| format!(" --session {}", shell_quote(session)))
            .unwrap_or_default();
        let takeover = if takeover { " --takeover" } else { "" };
        format!(
            "{bin}{session} terminal attach {}{takeover}",
            shell_quote(terminal_id)
        )
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

/// A terminal on one machine: herdr terminal ids are only unique per server.
type TerminalKey = On<TerminalId>;

fn terminal_key(machine: &MachineId, terminal_id: &TerminalId) -> TerminalKey {
    On::new(machine.clone(), terminal_id.clone())
}

/// How to attach each new terminal in a reconcile, by terminal id: the
/// command, or why it can't attach. A terminal missing from it waits (a
/// remote machine whose herdr isn't found yet).
pub type AttachCommands = HashMap<TerminalId, Result<String, String>>;

pub struct Terminals {
    /// The machine whose terminals `get`, `iter` and `show_only` see.
    selected: MachineId,
    views: HashMap<TerminalKey, TerminalView>,
    /// Spawn errors, shown in place of the terminal.
    failed: HashMap<TerminalKey, String>,
    /// Extra Ghostty config lines per terminal (later: a status shader).
    overrides: HashMap<TerminalKey, String>,
    config_dir: PathBuf,
    /// Terminals whose attach process has exited, already logged.
    detached: HashSet<TerminalKey>,
}

/// What changed in one reconcile.
#[derive(Debug, Default)]
pub struct Reconciled {
    pub spawned: Vec<TerminalId>,
    pub dropped: Vec<TerminalId>,
}

impl Terminals {
    pub fn new() -> Self {
        let config_dir = config_dir();
        Self {
            selected: MachineId::Local,
            views: HashMap::new(),
            failed: HashMap::new(),
            overrides: HashMap::new(),
            config_dir,
            detached: HashSet::new(),
        }
    }

    pub fn selected(&self) -> &MachineId {
        &self.selected
    }

    /// Points `get`, `iter` and `show_only` at another machine's terminals,
    /// hiding the previous machine's. Its views stay attached.
    pub fn select(&mut self, machine: &MachineId, cx: &mut App) {
        if &self.selected == machine {
            return;
        }
        self.hide_all(cx);
        self.selected = machine.clone();
    }

    /// Logs each terminal whose attach process exited, once per exit.
    pub fn report_detached(&mut self, cx: &App) {
        for (key, view) in &self.views {
            if view.terminal.read(cx).is_alive() {
                self.detached.remove(key);
            } else if self.detached.insert(key.clone()) {
                tracing::warn!(
                    "terminal {} on {} (pane {}): attach process exited",
                    key.id,
                    key.machine,
                    view.pane_id
                );
            }
        }
        self.detached.retain(|key| self.views.contains_key(key));
    }

    /// The selected machine's view of `terminal_id`.
    pub fn get(&self, terminal_id: &TerminalId) -> Option<&TerminalView> {
        self.views.get(&terminal_key(&self.selected, terminal_id))
    }

    /// `machine`'s view of `terminal_id`, selected or not.
    pub fn view_on(&self, machine: &MachineId, terminal_id: &TerminalId) -> Option<&TerminalView> {
        self.views.get(&terminal_key(machine, terminal_id))
    }

    /// Whether `machine` has a view (or a failure) for `terminal_id`.
    pub fn has(&self, machine: &MachineId, terminal_id: &TerminalId) -> bool {
        let key = terminal_key(machine, terminal_id);
        self.views.contains_key(&key) || self.failed.contains_key(&key)
    }

    /// Why the selected machine's `terminal_id` failed to spawn.
    pub fn failure(&self, terminal_id: &TerminalId) -> Option<&String> {
        self.failed.get(&terminal_key(&self.selected, terminal_id))
    }

    /// Views on every machine.
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// The selected machine's views, by terminal id.
    pub fn iter(&self) -> impl Iterator<Item = (&TerminalId, &TerminalView)> {
        self.views
            .iter()
            .filter(|(key, _)| key.machine == self.selected)
            .map(|(key, view)| (&key.id, view))
    }

    /// The machines that have views, other than the selected one.
    pub fn other_machines(&self) -> HashSet<MachineId> {
        self.views
            .keys()
            .chain(self.failed.keys())
            .map(|key| &key.machine)
            .filter(|machine| **machine != self.selected)
            .cloned()
            .collect()
    }

    /// The selected machine's terminal holding GPUI keyboard focus, if any.
    pub fn focused(&self, window: &Window, cx: &App) -> Option<&TerminalId> {
        self.iter()
            .find(|(_, view)| view.terminal.focus_handle(cx).is_focused(window))
            .map(|(id, _)| id)
    }

    /// Drops `machine`'s views whose terminal is gone from `snapshot`, then
    /// spawns views for its new terminals that `attach` has a command for
    /// (`None`: drop only). A view whose attach process died while its
    /// terminal still exists (e.g. another client took it over) is kept: the
    /// area shows it as detached and offers [`Self::reattach`], rather than
    /// fighting the other client.
    pub fn reconcile<T: 'static>(
        &mut self,
        machine: &MachineId,
        snapshot: &Snapshot,
        attach: Option<&AttachCommands>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Reconciled {
        let mut result = Reconciled::default();
        let live: HashMap<&TerminalId, &PaneInfo> = snapshot
            .panes
            .values()
            .map(|pane| (&pane.terminal_id, pane))
            .collect();
        let is_live = |key: &TerminalKey| &key.machine != machine || live.contains_key(&key.id);

        let gone: Vec<TerminalKey> = self
            .views
            .keys()
            .filter(|key| !is_live(key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(view) = self.views.remove(&key) {
                Self::discard(view, cx);
            }
            result.dropped.push(key.id);
        }
        self.failed.retain(|key, _| is_live(key));
        self.overrides.retain(|key, _| is_live(key));

        for (terminal_id, pane) in live {
            let key = terminal_key(machine, terminal_id);
            if let Some(view) = self.views.get_mut(&key) {
                view.pane_id.clone_from(&pane.pane_id);
                continue;
            }
            if self.failed.contains_key(&key) {
                continue;
            }
            let Some(command) = attach.and_then(|attach| attach.get(terminal_id)) else {
                continue;
            };
            match self.spawn(&key, pane, command.clone(), window, cx) {
                Ok(view) => {
                    self.views.insert(key, view);
                    result.spawned.push(terminal_id.clone());
                }
                Err(error) => {
                    tracing::error!(
                        "terminal {terminal_id} on {machine} (pane {}): {error:#}",
                        pane.pane_id
                    );
                    self.failed.insert(key, format!("{error:#}"));
                }
            }
        }
        result
    }

    /// Drops every view on `machine` (it was removed or restarted).
    pub fn forget_machine(&mut self, machine: &MachineId, cx: &mut App) {
        let keys: Vec<TerminalKey> = self
            .views
            .keys()
            .filter(|key| &key.machine == machine)
            .cloned()
            .collect();
        for key in keys {
            if let Some(view) = self.views.remove(&key) {
                Self::discard(view, cx);
            }
        }
        self.failed.retain(|key, _| &key.machine != machine);
        self.overrides.retain(|key, _| &key.machine != machine);
    }

    fn discard(view: TerminalView, cx: &mut App) {
        // Dropping the entity frees the native surface and ends the attach
        // process; hide first so nothing lingers on screen.
        view.terminal
            .update(cx, |terminal, _| terminal.set_visible(false));
        let _ = std::fs::remove_file(&view.config_path);
    }

    /// Whether the selected machine's terminal's attach process is running.
    pub fn is_attached(&self, terminal_id: &TerminalId, cx: &App) -> bool {
        self.get(terminal_id)
            .is_some_and(|view| view.terminal.read(cx).is_alive())
    }

    /// `machine`'s terminals whose attach process has ended.
    pub fn detached_on(&self, machine: &MachineId, cx: &App) -> Vec<TerminalId> {
        self.views
            .iter()
            .filter(|(key, view)| &key.machine == machine && !view.terminal.read(cx).is_alive())
            .map(|(key, _)| key.id.clone())
            .collect()
    }

    /// Replaces a detached (or failed) view on `machine` with a fresh attach.
    pub fn reattach<T: 'static>(
        &mut self,
        machine: &MachineId,
        pane: &PaneInfo,
        command: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        let key = terminal_key(machine, &pane.terminal_id);
        if let Some(view) = self.views.remove(&key) {
            view.terminal
                .update(cx, |terminal, _| terminal.set_visible(false));
        }
        self.failed.remove(&key);
        match self.spawn(&key, pane, command, window, cx) {
            Ok(view) => {
                self.views.insert(key, view);
            }
            Err(error) => {
                self.failed.insert(key, format!("{error:#}"));
            }
        }
    }

    fn spawn<T: 'static>(
        &self,
        key: &TerminalKey,
        pane: &PaneInfo,
        command: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> anyhow::Result<TerminalView> {
        let command = command.map_err(anyhow::Error::msg)?;
        let config_path = self.write_config(key, cx)?;
        // A remote pane's folder is on the remote machine.
        let cwd = pane
            .working_dir()
            .filter(|_| key.machine.is_local())
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut options = TerminalOptions::new(command, cwd);
        options.configuration = TerminalConfiguration::File(config_path.clone());
        options.focus_on_spawn = false;
        options.quiet_login = true;
        let terminal = Terminal::spawn(options, window, cx).map_err(anyhow::Error::msg)?;
        // Hidden until the layout places it.
        terminal.update(cx, |terminal, _| terminal.set_visible(false));
        Ok(TerminalView {
            terminal,
            pane_id: pane.pane_id.clone(),
            config_path,
            shown: false,
        })
    }

    /// Shows exactly the terminals in `visible` and hides the rest. A native
    /// view that drops out of the layout keeps its last frame, so it has to be
    /// hidden explicitly; a shown one appears at its next prepaint.
    /// `visible` names the selected machine's terminals.
    pub fn show_only(&mut self, visible: &HashSet<&TerminalId>, cx: &mut App) {
        for (key, view) in &mut self.views {
            let show = key.machine == self.selected && visible.contains(&key.id);
            if view.shown != show {
                view.shown = show;
                view.terminal
                    .update(cx, |terminal, _| terminal.set_visible(show));
            }
        }
    }

    /// Hides every terminal (e.g. while nothing is connected).
    pub fn hide_all(&mut self, cx: &mut App) {
        self.show_only(&HashSet::new(), cx);
    }

    /// Replaces one terminal's extra config lines and reloads it in place.
    /// For one of the selected machine's terminals.
    pub fn set_overrides(&mut self, terminal_id: &TerminalId, lines: String, cx: &mut App) {
        let key = terminal_key(&self.selected, terminal_id);
        self.overrides.insert(key.clone(), lines);
        self.reload(Some(&key), cx);
    }

    /// Rewrites config files (all, or one) and reloads them without
    /// restarting the terminals. Called when the theme changes.
    pub fn reload_all(&mut self, cx: &mut App) {
        self.reload(None, cx);
    }

    fn reload(&mut self, only: Option<&TerminalKey>, cx: &mut App) {
        let keys: Vec<TerminalKey> = self
            .views
            .keys()
            .filter(|key| only.is_none_or(|only| only == *key))
            .cloned()
            .collect();
        for key in keys {
            let id = &key.id;
            let path = match self.write_config(&key, cx) {
                Ok(path) => path,
                Err(error) => {
                    tracing::error!("ghostty config for {id}: {error:#}");
                    continue;
                }
            };
            if let Some(view) = self.views.get(&key) {
                let result = view
                    .terminal
                    .update(cx, |terminal, _| terminal.load_config_file(&path));
                if let Err(error) = result {
                    tracing::error!("reload ghostty config for {id}: {error}");
                }
            }
        }
    }

    /// The terminal's own config file: the user's Ghostty settings for the
    /// app, then colors from the app theme, then per-terminal overrides.
    fn write_config(&self, key: &TerminalKey, cx: &App) -> anyhow::Result<PathBuf> {
        let contents = self.config(Some(key), cx)?;
        let path = self.config_dir.join(format!(
            "{}-{}.ghostty",
            sanitize(&key.machine.to_string()),
            sanitize(&key.id)
        ));
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    fn config(&self, key: Option<&TerminalKey>, cx: &App) -> anyhow::Result<String> {
        let mut contents = base_config(&self.config_dir, cx)?;
        if let Some(extra) = key.and_then(|key| self.overrides.get(key)) {
            contents.push_str(extra);
            contents.push('\n');
        }
        Ok(contents)
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

/// Where the app writes its terminals' Ghostty config files, for this
/// process.
fn config_dir() -> PathBuf {
    std::env::temp_dir()
        .join(format!("grasslands-{}", std::process::id()))
        .join("ghostty")
}

/// The user's Ghostty settings, then the theme's.
fn base_config(config_dir: &Path, cx: &App) -> anyhow::Result<String> {
    std::fs::create_dir_all(config_dir)?;
    let mut contents = String::new();
    if let Some(user) = user_config_path().filter(|path| path.is_file()) {
        contents.push_str(&std::fs::read_to_string(&user)?);
        contents.push('\n');
    }
    contents.push_str(&theme_config(cx));
    Ok(contents)
}

/// A terminal running `command` outside herdr (a setup fix, a machine
/// sign-in), themed like the panes. It keeps its output on screen after the
/// command ends.
pub fn spawn_command<T: 'static>(
    command: String,
    window: &mut Window,
    cx: &mut Context<T>,
) -> anyhow::Result<Entity<Terminal>> {
    let dir = config_dir();
    let mut config = base_config(&dir, cx)?;
    config.push_str("wait-after-command = true\n");
    let path = dir.join(format!("{COMMAND_TERMINAL}.ghostty"));
    std::fs::write(&path, config)?;
    let cwd = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let mut options = TerminalOptions::new(command, cwd);
    options.configuration = TerminalConfiguration::File(path);
    options.quiet_login = true;
    Terminal::spawn(options, window, cx).map_err(anyhow::Error::msg)
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `~/.config/grasslands/ghostty.conf`: Ghostty settings for every terminal
/// in the app (font, cursor, ...). Not under `XDG_CONFIG_HOME`, which the dev
/// scripts point at herdr's isolated config.
pub fn user_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".config/grasslands/ghostty.conf"))
}

fn theme_config(cx: &App) -> String {
    let theme = cx.theme();
    // Ghostty colors are opaque and the theme's selection is translucent, so
    // flatten it over the background; selected text keeps its own colors.
    format!(
        "background = {}\nforeground = {}\ncursor-color = {}\nselection-background = {}\nselection-foreground = cell-foreground\nwindow-padding-x = 4\nwindow-padding-y = 2\n",
        hex(theme.background),
        hex(theme.foreground),
        hex(theme.primary),
        hex(theme.background.blend(theme.selection)),
    )
}

fn hex(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b)
    )
}

#[cfg(test)]
mod tests {
    // Not `super::*`: with test-support, GPUI's glob brings its own `test`
    // macro, which shadows the built-in one.
    use super::{Launch, hex};
    use gpui_kit::{Hsla, rgb};
    use std::path::PathBuf;

    #[test]
    fn attach_command_names_the_session_when_one_is_set() {
        let launch = Launch {
            bin: PathBuf::from("/opt/herdr/herdr"),
            session: Some("dev-x".to_owned()),
        };
        assert_eq!(
            launch.attach_command(&"term_1".into(), false),
            "/opt/herdr/herdr --session dev-x terminal attach term_1"
        );
    }

    #[test]
    fn attach_command_takes_over_only_when_asked() {
        let launch = Launch {
            bin: PathBuf::from("/opt/herdr/herdr"),
            session: None,
        };
        assert_eq!(
            launch.attach_command(&"term_1".into(), true),
            "/opt/herdr/herdr terminal attach term_1 --takeover"
        );
    }

    #[test]
    fn attach_command_quotes_paths_with_spaces() {
        let launch = Launch {
            bin: PathBuf::from("/Applications/My Tools/herdr"),
            session: None,
        };
        assert_eq!(
            launch.attach_command(&"term_1".into(), false),
            "'/Applications/My Tools/herdr' terminal attach term_1"
        );
    }

    #[test]
    fn hex_formats_theme_colors_as_ghostty_rgb() {
        assert_eq!(hex(Hsla::from(rgb(0x10182a))), "#10182a");
    }
}
