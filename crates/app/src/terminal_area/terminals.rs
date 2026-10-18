//! One Ghostty view per herdr terminal.
//!
//! Views are keyed by `terminal_id`, not pane id: a pane moved to another tab
//! or workspace keeps its terminal (and so its view and process), while a
//! herdr restart hands out new terminal ids and so gets fresh views. The set
//! is reconciled against every snapshot herdr-client delivers, which follows
//! `pane.created`, `pane.closed` and `pane.moved` within one re-read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use gpui_libghostty::{TerminalConfiguration, TerminalOptions};

use crate::herdr::{self, PaneInfo, Snapshot};

gpui_libghostty::bind_gpui!(gpui_kit);

pub struct TerminalView {
    pub terminal: Entity<Terminal>,
    pub pane_id: String,
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

    fn attach_command(&self, terminal_id: &str) -> String {
        let bin = shell_quote(&self.bin.to_string_lossy());
        match &self.session {
            Some(session) => format!(
                "{bin} --session {} terminal attach {} --takeover",
                shell_quote(session),
                shell_quote(terminal_id)
            ),
            None => format!(
                "{bin} terminal attach {} --takeover",
                shell_quote(terminal_id)
            ),
        }
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

pub struct Terminals {
    /// `None` when the app refused its herdr endpoint: nothing is attached,
    /// so a guarded build can never reach the wrong session.
    launch: Option<Launch>,
    views: HashMap<String, TerminalView>,
    /// Spawn errors by terminal id, shown in place of the terminal.
    pub failed: HashMap<String, String>,
    /// Extra Ghostty config lines per terminal (later: a status shader).
    overrides: HashMap<String, String>,
    config_dir: PathBuf,
    /// Terminals whose attach process has exited, already logged.
    detached: HashSet<String>,
}

/// What changed in one reconcile.
#[derive(Debug, Default)]
pub struct Reconciled {
    pub spawned: Vec<String>,
    pub dropped: Vec<String>,
}

impl Terminals {
    pub fn new(launch: Option<Launch>) -> Self {
        let config_dir = std::env::temp_dir()
            .join(format!("grasslands-{}", std::process::id()))
            .join("ghostty");
        Self {
            launch,
            views: HashMap::new(),
            failed: HashMap::new(),
            overrides: HashMap::new(),
            config_dir,
            detached: HashSet::new(),
        }
    }

    /// Logs each terminal whose attach process exited, once per exit.
    pub fn report_detached(&mut self, cx: &App) {
        for (id, view) in &self.views {
            if view.terminal.read(cx).is_alive() {
                self.detached.remove(id);
            } else if self.detached.insert(id.clone()) {
                tracing::warn!(
                    "terminal {id} (pane {}): attach process exited",
                    view.pane_id
                );
            }
        }
        self.detached.retain(|id| self.views.contains_key(id));
    }

    pub fn get(&self, terminal_id: &str) -> Option<&TerminalView> {
        self.views.get(terminal_id)
    }

    pub fn len(&self) -> usize {
        self.views.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &TerminalView)> {
        self.views.iter()
    }

    /// The terminal id holding GPUI keyboard focus, if any.
    pub fn focused(&self, window: &Window, cx: &App) -> Option<&str> {
        self.views
            .iter()
            .find(|(_, view)| view.terminal.focus_handle(cx).is_focused(window))
            .map(|(id, _)| id.as_str())
    }

    /// Spawns views for new terminals and drops views whose terminal is gone.
    /// A view whose attach process died while its terminal still exists (e.g.
    /// another client took it over) is kept: the area shows it as detached
    /// and offers [`Self::reattach`], rather than fighting the other client.
    pub fn reconcile<T: 'static>(
        &mut self,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Reconciled {
        let mut result = Reconciled::default();
        let live: HashMap<&str, &PaneInfo> = snapshot
            .panes
            .values()
            .map(|pane| (pane.terminal_id.as_str(), pane))
            .collect();

        let gone: Vec<String> = self
            .views
            .iter()
            .filter(|(id, _)| !live.contains_key(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            if let Some(view) = self.views.remove(&id) {
                // Dropping the entity frees the native surface and ends the
                // attach process; hide first so nothing lingers on screen.
                view.terminal
                    .update(cx, |terminal, _| terminal.set_visible(false));
                let _ = std::fs::remove_file(&view.config_path);
            }
            result.dropped.push(id);
        }
        self.failed.retain(|id, _| live.contains_key(id.as_str()));
        self.overrides
            .retain(|id, _| live.contains_key(id.as_str()));

        for (terminal_id, pane) in live {
            if let Some(view) = self.views.get_mut(terminal_id) {
                view.pane_id.clone_from(&pane.pane_id);
                continue;
            }
            if self.failed.contains_key(terminal_id) {
                continue;
            }
            match self.spawn(pane, window, cx) {
                Ok(view) => {
                    self.views.insert(terminal_id.to_owned(), view);
                    result.spawned.push(terminal_id.to_owned());
                }
                Err(error) => {
                    tracing::error!("terminal {terminal_id} (pane {}): {error:#}", pane.pane_id);
                    self.failed
                        .insert(terminal_id.to_owned(), format!("{error:#}"));
                }
            }
        }
        result
    }

    /// Whether the terminal's attach process is still running.
    pub fn is_attached(&self, terminal_id: &str, cx: &App) -> bool {
        self.views
            .get(terminal_id)
            .is_some_and(|view| view.terminal.read(cx).is_alive())
    }

    /// Replaces a detached (or failed) view with a fresh attach.
    pub fn reattach<T: 'static>(
        &mut self,
        pane: &PaneInfo,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        if let Some(view) = self.views.remove(&pane.terminal_id) {
            view.terminal
                .update(cx, |terminal, _| terminal.set_visible(false));
        }
        self.failed.remove(&pane.terminal_id);
        match self.spawn(pane, window, cx) {
            Ok(view) => {
                self.views.insert(pane.terminal_id.clone(), view);
            }
            Err(error) => {
                self.failed
                    .insert(pane.terminal_id.clone(), format!("{error:#}"));
            }
        }
    }

    fn spawn<T: 'static>(
        &self,
        pane: &PaneInfo,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> anyhow::Result<TerminalView> {
        let launch = self
            .launch
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("herdr is disconnected"))?;
        let config_path = self.write_config(&pane.terminal_id, cx)?;
        let cwd = pane
            .working_dir()
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut options = TerminalOptions::new(launch.attach_command(&pane.terminal_id), cwd);
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
    pub fn show_only(&mut self, visible: &HashSet<&str>, cx: &mut App) {
        for (id, view) in &mut self.views {
            let show = visible.contains(id.as_str());
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
    pub fn set_overrides(&mut self, terminal_id: &str, lines: String, cx: &mut App) {
        self.overrides.insert(terminal_id.to_owned(), lines);
        self.reload(Some(terminal_id), cx);
    }

    /// Rewrites config files (all, or one) and reloads them without
    /// restarting the terminals. Called when the theme changes.
    pub fn reload(&mut self, only: Option<&str>, cx: &mut App) {
        let ids: Vec<String> = self
            .views
            .keys()
            .filter(|id| only.is_none_or(|only| only == id.as_str()))
            .cloned()
            .collect();
        for id in ids {
            let path = match self.write_config(&id, cx) {
                Ok(path) => path,
                Err(error) => {
                    tracing::error!("ghostty config for {id}: {error:#}");
                    continue;
                }
            };
            if let Some(view) = self.views.get(&id) {
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
    fn write_config(&self, terminal_id: &str, cx: &App) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(&self.config_dir)?;
        let mut contents = String::new();
        if let Some(user) = user_config_path().filter(|path| path.is_file()) {
            contents.push_str(&std::fs::read_to_string(&user)?);
            contents.push('\n');
        }
        contents.push_str(&theme_config(cx));
        if let Some(extra) = self.overrides.get(terminal_id) {
            contents.push_str(extra);
            contents.push('\n');
        }
        let path = self
            .config_dir
            .join(format!("{}.ghostty", sanitize(terminal_id)));
        std::fs::write(&path, contents)?;
        Ok(path)
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
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
            launch.attach_command("term_1"),
            "/opt/herdr/herdr --session dev-x terminal attach term_1 --takeover"
        );
    }

    #[test]
    fn attach_command_quotes_paths_with_spaces() {
        let launch = Launch {
            bin: PathBuf::from("/Applications/My Tools/herdr"),
            session: None,
        };
        assert_eq!(
            launch.attach_command("term_1"),
            "'/Applications/My Tools/herdr' terminal attach term_1 --takeover"
        );
    }

    #[test]
    fn hex_formats_theme_colors_as_ghostty_rgb() {
        assert_eq!(hex(Hsla::from(rgb(0x10182a))), "#10182a");
    }
}
