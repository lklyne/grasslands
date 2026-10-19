//! herdr's `config.toml`: where herdr reads it, the preferences the app
//! follows, edits that keep the user's comments and formatting, a watcher,
//! and `herdr config check`. herdr has no socket method to read or write its
//! config; the file is the source of truth, and `server.reload_config`
//! (`Client::server_reload_config`) makes the server apply an edit.
//!
//! herdr 0.9.5 rejects the whole file when any value has the wrong type or an
//! unknown variant ("…; using defaults"), and ignores unknown keys with a
//! warning. [`parse`] does the same for the keys it knows, so the app never
//! follows a value herdr itself isn't using. See
//! docs/decisions/0021-herdr-prefs.md.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// Overrides the config file's path, as for herdr.
pub const CONFIG_PATH_ENV: &str = "HERDR_CONFIG_PATH";
/// Set (non-empty) to silence herdr's agent sounds whatever the config says.
pub const DISABLE_SOUND_ENV: &str = "HERDR_DISABLE_SOUND";

/// The agents `[ui.sound.agents]` takes a key for in herdr 0.9.5 (others are
/// "unknown config key"), in herdr's order. `droid` is muted by default.
pub const SOUND_AGENTS: [&str; 20] = [
    "pi", "claude", "codex", "gemini", "cursor", "devin", "agy", "cline", "kimi", "kiro", "droid",
    "amp", "grok", "hermes", "kilo", "qodercli", "qwen", "letta", "maki", "muse",
];

/// Agents whose sound is off unless the config turns it on.
const MUTED_BY_DEFAULT: [&str; 1] = ["droid"];

/// The most `ui.toast.delay_seconds` herdr accepts.
pub const MAX_TOAST_DELAY: u64 = 3600;

/// The config file herdr reads: `HERDR_CONFIG_PATH`, else
/// `$XDG_CONFIG_HOME/herdr/config.toml`, else `~/.config/herdr/config.toml`.
/// `None` without any of them.
pub fn config_path() -> Option<PathBuf> {
    config_path_with(|name| std::env::var_os(name))
}

/// [`config_path`] over any environment. An empty variable counts as unset,
/// as `Endpoint::from_env` treats them.
pub fn config_path_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |name: &str| var(name).filter(|value| !value.is_empty());
    if let Some(path) = var(CONFIG_PATH_ENV) {
        return Some(PathBuf::from(path));
    }
    let config_home = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("herdr").join("config.toml"))
}

/// Whether `HERDR_DISABLE_SOUND` silences agent sounds in this process.
pub fn sound_disabled_by_env() -> bool {
    std::env::var_os(DISABLE_SOUND_ENV).is_some_and(|value| !value.is_empty())
}

macro_rules! keyed_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $key:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            /// The value herdr's config spells it as.
            pub fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key),+
                }
            }

            pub fn from_key(key: &str) -> Option<Self> {
                match key {
                    $($key => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

keyed_enum!(
    /// `[ui.sound.agents] <agent>`.
    AgentSound {
        /// herdr's default for the agent (on, except droid).
        #[default]
        Default => "default",
        On => "on",
        Off => "off",
    }
);

keyed_enum!(
    /// `ui.toast.delivery`: where herdr sends finished/needs-input popups.
    ToastDelivery {
        #[default]
        Off => "off",
        Herdr => "herdr",
        Terminal => "terminal",
        System => "system",
    }
);

keyed_enum!(
    /// `ui.status_indicators`.
    StatusIndicators {
        #[default]
        Dots => "dots",
        Symbols => "symbols",
    }
);

/// `ui.agent_panel_sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AgentPanelSort {
    /// Grouped by space ("spaces"; "workspaces" is an alias).
    #[default]
    Spaces,
    /// The attention queue.
    Priority,
}

impl AgentPanelSort {
    pub const ALL: &[Self] = &[Self::Spaces, Self::Priority];

    /// The value herdr saves.
    pub fn key(self) -> &'static str {
        match self {
            Self::Spaces => "spaces",
            Self::Priority => "priority",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "spaces" | "workspaces" => Some(Self::Spaces),
            "priority" => Some(Self::Priority),
            _ => None,
        }
    }
}

/// `[ui.sound]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundPrefs {
    pub enabled: bool,
    /// One mp3 for every agent sound, resolved against the config's folder.
    pub path: Option<PathBuf>,
    /// Overrides `path` for finished agents.
    pub done_path: Option<PathBuf>,
    /// Overrides `path` for agents that need input.
    pub request_path: Option<PathBuf>,
    /// `[ui.sound.agents]`, as written (only the agents set).
    pub agents: BTreeMap<String, AgentSound>,
}

impl Default for SoundPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            done_path: None,
            request_path: None,
            agents: BTreeMap::new(),
        }
    }
}

impl SoundPrefs {
    /// The override for `agent` (a herdr agent id such as `claude`).
    pub fn agent(&self, agent: &str) -> AgentSound {
        self.agents.get(agent).copied().unwrap_or_default()
    }

    /// Whether an agent of kind `agent` makes a sound when its state changes
    /// in a background workspace. `enabled = false` silences every agent;
    /// otherwise the per-agent override wins, and `default` is on except
    /// for droid. Agents of unknown kind follow `enabled`.
    pub fn plays_for(&self, agent: Option<&str>) -> bool {
        if !self.enabled {
            return false;
        }
        match agent {
            Some(agent) => match self.agent(agent) {
                AgentSound::On => true,
                AgentSound::Off => false,
                AgentSound::Default => !MUTED_BY_DEFAULT.contains(&agent),
            },
            None => true,
        }
    }

    /// The custom mp3 for an agent that `done` (else needs input), if set.
    pub fn custom_sound(&self, done: bool) -> Option<&Path> {
        let specific = if done {
            &self.done_path
        } else {
            &self.request_path
        };
        specific.as_deref().or(self.path.as_deref())
    }
}

/// `[ui.toast]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToastPrefs {
    pub delivery: ToastDelivery,
    /// Seconds to wait; herdr notifies only if the pane is still in the
    /// same state then.
    pub delay_seconds: u64,
}

impl Default for ToastPrefs {
    fn default() -> Self {
        Self {
            delivery: ToastDelivery::Off,
            delay_seconds: 1,
        }
    }
}

/// The herdr preferences the app follows, with herdr's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub sound: SoundPrefs,
    pub toast: ToastPrefs,
    pub agent_panel_sort: AgentPanelSort,
    pub status_indicators: StatusIndicators,
    /// Ask before closing a workspace.
    pub confirm_close: bool,
    /// Ask for a tab name before creating a tab.
    pub prompt_new_tab_name: bool,
    /// Ask for a workspace name before creating a workspace.
    pub prompt_new_workspace_name: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            sound: SoundPrefs::default(),
            toast: ToastPrefs::default(),
            agent_panel_sort: AgentPanelSort::default(),
            status_indicators: StatusIndicators::default(),
            confirm_close: true,
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
        }
    }
}

/// What [`parse`] made of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    /// What herdr runs with: the file's values, or every default when herdr
    /// would reject the file.
    pub prefs: Prefs,
    /// What was wrong, one line each.
    pub problems: Vec<String>,
    /// herdr falls back to its defaults for the whole file.
    pub using_defaults: bool,
}

/// Reads the preferences from config text. Relative sound paths resolve
/// against `config_dir`. Never fails: a file that doesn't parse, or a value
/// of the wrong type, gives herdr's defaults (herdr does the same) and a
/// problem line.
pub fn parse(text: &str, config_dir: Option<&Path>) -> Parsed {
    let doc = match text.parse::<DocumentMut>() {
        Ok(doc) => doc,
        Err(error) => {
            return Parsed {
                prefs: Prefs::default(),
                problems: vec![format!("config.toml doesn't parse: {}", error.message())],
                using_defaults: true,
            };
        }
    };
    let mut reader = Reader {
        root: doc.as_item(),
        problems: Vec::new(),
    };
    let defaults = Prefs::default();
    let resolve = |path: String| {
        let path = PathBuf::from(path);
        match config_dir {
            Some(dir) if path.is_relative() => dir.join(path),
            _ => path,
        }
    };
    let mut agents = BTreeMap::new();
    if let Some(table) = reader.get(&["ui", "sound", "agents"]).and_then(Item::as_table_like) {
        for (agent, item) in table.iter() {
            let key = format!("ui.sound.agents.{agent}");
            if !SOUND_AGENTS.contains(&agent) {
                reader.problems.push(format!("unknown config key {key}; ignored"));
                continue;
            }
            if let Some(sound) = reader.variant(&key, item, AgentSound::from_key) {
                agents.insert(agent.to_owned(), sound);
            }
        }
    }
    let prefs = Prefs {
        sound: SoundPrefs {
            enabled: reader
                .bool(&["ui", "sound", "enabled"])
                .unwrap_or(defaults.sound.enabled),
            path: reader.string(&["ui", "sound", "path"]).map(resolve),
            done_path: reader.string(&["ui", "sound", "done_path"]).map(resolve),
            request_path: reader.string(&["ui", "sound", "request_path"]).map(resolve),
            agents,
        },
        toast: ToastPrefs {
            delivery: reader
                .enumerated(&["ui", "toast", "delivery"], ToastDelivery::from_key)
                .unwrap_or(defaults.toast.delivery),
            delay_seconds: reader
                .delay(&["ui", "toast", "delay_seconds"])
                .unwrap_or(defaults.toast.delay_seconds),
        },
        agent_panel_sort: reader
            .enumerated(&["ui", "agent_panel_sort"], AgentPanelSort::from_key)
            .unwrap_or(defaults.agent_panel_sort),
        status_indicators: reader
            .enumerated(&["ui", "status_indicators"], StatusIndicators::from_key)
            .unwrap_or(defaults.status_indicators),
        confirm_close: reader
            .bool(&["ui", "confirm_close"])
            .unwrap_or(defaults.confirm_close),
        prompt_new_tab_name: reader
            .bool(&["ui", "prompt_new_tab_name"])
            .unwrap_or(defaults.prompt_new_tab_name),
        prompt_new_workspace_name: reader
            .bool(&["ui", "prompt_new_workspace_name"])
            .unwrap_or(defaults.prompt_new_workspace_name),
    };
    let rejected = reader.rejected();
    Parsed {
        prefs: if rejected { defaults } else { prefs },
        problems: reader.problems,
        using_defaults: rejected,
    }
}

/// Reads the config at `path`. A missing file is herdr's defaults with no
/// problems; an unreadable one is the defaults with a problem.
pub fn load(path: &Path) -> Parsed {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, path.parent()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Parsed::default(),
        Err(error) => Parsed {
            problems: vec![format!("can't read {}: {error}", path.display())],
            using_defaults: true,
            ..Parsed::default()
        },
    }
}

/// Typed lookups that record what was wrong.
struct Reader<'a> {
    root: &'a Item,
    problems: Vec<String>,
}

/// A problem that makes herdr reject the file starts with this.
const INVALID: &str = "invalid value for ";

impl<'a> Reader<'a> {
    fn get(&self, path: &[&str]) -> Option<&'a Item> {
        path.iter()
            .try_fold(self.root, |item, key| item.get(key))
            .filter(|item| !item.is_none())
    }

    fn invalid(&mut self, key: &str, expected: &str) {
        self.problems
            .push(format!("{INVALID}{key}: expected {expected}"));
    }

    fn rejected(&self) -> bool {
        self.problems.iter().any(|p| p.starts_with(INVALID))
    }

    fn bool(&mut self, path: &[&str]) -> Option<bool> {
        let item = self.get(path)?;
        let value = item.as_bool();
        if value.is_none() {
            self.invalid(&path.join("."), "true or false");
        }
        value
    }

    fn string(&mut self, path: &[&str]) -> Option<String> {
        let item = self.get(path)?;
        let value = item.as_str().map(str::to_owned);
        if value.is_none() {
            self.invalid(&path.join("."), "a string");
        }
        value
    }

    fn enumerated<T>(&mut self, path: &[&str], from_key: impl Fn(&str) -> Option<T>) -> Option<T> {
        let item = self.get(path)?;
        self.variant(&path.join("."), item, from_key)
    }

    fn variant<T>(
        &mut self,
        key: &str,
        item: &Item,
        from_key: impl Fn(&str) -> Option<T>,
    ) -> Option<T> {
        let value = item.as_str().and_then(from_key);
        if value.is_none() {
            self.invalid(key, "one of herdr's documented values");
        }
        value
    }

    fn delay(&mut self, path: &[&str]) -> Option<u64> {
        let item = self.get(path)?;
        let value = item
            .as_integer()
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n <= MAX_TOAST_DELAY);
        if value.is_none() {
            self.invalid(
                &path.join("."),
                &format!("a whole number of seconds, 0 to {MAX_TOAST_DELAY}"),
            );
        }
        value
    }
}

/// One preference change, written by [`write`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    SoundEnabled(bool),
    /// `Default` removes the agent's key.
    AgentSound { agent: String, sound: AgentSound },
    ToastDelivery(ToastDelivery),
    /// Clamped to herdr's 0–3600.
    ToastDelay(u64),
    AgentPanelSort(AgentPanelSort),
    StatusIndicators(StatusIndicators),
    ConfirmClose(bool),
    PromptNewTabName(bool),
    PromptNewWorkspaceName(bool),
}

impl Edit {
    /// The key's path, and its new value (`None` removes it).
    fn target(&self) -> (Vec<&str>, Option<Value>) {
        match self {
            Self::SoundEnabled(on) => (vec!["ui", "sound", "enabled"], Some((*on).into())),
            Self::AgentSound { agent, sound } => (
                vec!["ui", "sound", "agents", agent.as_str()],
                (*sound != AgentSound::Default).then(|| sound.key().into()),
            ),
            Self::ToastDelivery(delivery) => {
                (vec!["ui", "toast", "delivery"], Some(delivery.key().into()))
            }
            Self::ToastDelay(seconds) => {
                let seconds = i64::try_from((*seconds).min(MAX_TOAST_DELAY)).unwrap_or(0);
                (vec!["ui", "toast", "delay_seconds"], Some(seconds.into()))
            }
            Self::AgentPanelSort(sort) => (vec!["ui", "agent_panel_sort"], Some(sort.key().into())),
            Self::StatusIndicators(indicators) => (
                vec!["ui", "status_indicators"],
                Some(indicators.key().into()),
            ),
            Self::ConfirmClose(on) => (vec!["ui", "confirm_close"], Some((*on).into())),
            Self::PromptNewTabName(on) => (vec!["ui", "prompt_new_tab_name"], Some((*on).into())),
            Self::PromptNewWorkspaceName(on) => (
                vec!["ui", "prompt_new_workspace_name"],
                Some((*on).into()),
            ),
        }
    }

    /// Applies the edit to the prefs in memory, as rereading the written
    /// file would.
    pub fn apply_to(&self, prefs: &mut Prefs) {
        match self {
            Self::SoundEnabled(on) => prefs.sound.enabled = *on,
            Self::AgentSound { agent, sound } => {
                if *sound == AgentSound::Default {
                    prefs.sound.agents.remove(agent);
                } else {
                    prefs.sound.agents.insert(agent.clone(), *sound);
                }
            }
            Self::ToastDelivery(delivery) => prefs.toast.delivery = *delivery,
            Self::ToastDelay(seconds) => prefs.toast.delay_seconds = (*seconds).min(MAX_TOAST_DELAY),
            Self::AgentPanelSort(sort) => prefs.agent_panel_sort = *sort,
            Self::StatusIndicators(indicators) => prefs.status_indicators = *indicators,
            Self::ConfirmClose(on) => prefs.confirm_close = *on,
            Self::PromptNewTabName(on) => prefs.prompt_new_tab_name = *on,
            Self::PromptNewWorkspaceName(on) => prefs.prompt_new_workspace_name = *on,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file isn't TOML, so an edit can't keep it intact.
    #[error("config.toml doesn't parse, so it was left alone: {0}")]
    Parse(String),
    /// A key on the way is a value, not a table.
    #[error("{0} isn't a table in config.toml")]
    NotATable(String),
}

/// Applies `edit` to config text, keeping every comment, blank line and
/// key order. Missing tables are added; `[ui]` stays implicit when only its
/// subtables need it.
pub fn apply(text: &str, edit: &Edit) -> Result<String, ConfigError> {
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| ConfigError::Parse(error.message().to_owned()))?;
    let (path, value) = edit.target();
    let Some((leaf, tables)) = path.split_last() else {
        return Ok(text.to_owned());
    };
    let mut item: &mut Item = doc.as_item_mut();
    for (depth, key) in tables.iter().enumerate() {
        let inline = item.is_inline_table();
        let table = item
            .as_table_like_mut()
            .ok_or_else(|| ConfigError::NotATable(path[..depth].join(".")))?;
        if !table.contains_key(key) {
            if value.is_none() {
                return Ok(text.to_owned());
            }
            let child = if inline {
                Item::Value(Value::InlineTable(InlineTable::new()))
            } else {
                let mut child = Table::new();
                child.set_implicit(true);
                Item::Table(child)
            };
            table.insert(key, child);
        }
        item = table
            .get_mut(key)
            .ok_or_else(|| ConfigError::NotATable(path[..=depth].join(".")))?;
    }
    let table = item
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::NotATable(tables.join(".")))?;
    match value {
        Some(value) => match table.get_mut(leaf) {
            // Keep the key's own decoration (comments, spacing) when replacing.
            Some(Item::Value(old)) => {
                let decor = old.decor().clone();
                *old = value;
                *old.decor_mut() = decor;
            }
            _ => {
                table.insert(leaf, Item::Value(value));
            }
        },
        None => {
            table.remove(leaf);
        }
    }
    Ok(doc.to_string())
}

/// Writes `edit` into the config at `path`, creating the file (and its
/// folder) when missing. A symlinked config is edited at its target. The
/// new text is written next to the file and renamed over it, so herdr never
/// reads half a file.
pub fn write(path: &Path, edit: &Edit) -> Result<(), ConfigError> {
    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ConfigError::Io { path, source }
    };
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = match std::fs::read_to_string(&target) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(io_error(&target)(error)),
    };
    let updated = apply(&text, edit)?;
    if updated == text {
        return Ok(());
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(io_error(dir))?;
    }
    let file_name = target
        .file_name()
        .map_or_else(|| "config.toml".into(), |name| name.to_string_lossy());
    let temp = target.with_file_name(format!(".{file_name}.grasslands-{}", std::process::id()));
    std::fs::write(&temp, updated).map_err(io_error(&temp))?;
    if let Ok(metadata) = std::fs::metadata(&target) {
        // Keep the user's permissions on the file.
        let _ = std::fs::set_permissions(&temp, metadata.permissions());
    }
    std::fs::rename(&temp, &target).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        io_error(&target)(error)
    })
}

/// Watches the config file for changes from anywhere (an editor, herdr, the
/// app itself) and sends `()` for each. It watches the file's folder, since
/// editors replace files by renaming, and the file may not exist yet; a
/// symlinked config's target folder is watched too.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn new(path: &Path, changes: async_channel::Sender<()>) -> notify::Result<Self> {
        let mut names: Vec<PathBuf> = vec![path.to_path_buf()];
        if let Ok(target) = std::fs::canonicalize(path)
            && target != path
        {
            names.push(target);
        }
        let watched_names: Vec<OsString> = names
            .iter()
            .filter_map(|p| p.file_name().map(ToOwned::to_owned))
            .collect();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        log::warn!("herdr config watch error: {error}");
                        return;
                    }
                };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let ours = event.paths.iter().any(|path| {
                    path.file_name()
                        .is_some_and(|name| watched_names.iter().any(|n| n == name))
                });
                if ours {
                    // The receiver is gone only when the app is; nothing to do then.
                    let _ = changes.try_send(());
                }
            })?;
        let mut watched_any = false;
        for dir in names.iter().filter_map(|p| p.parent()) {
            match watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => watched_any = true,
                Err(error) => log::warn!("can't watch {}: {error}", dir.display()),
            }
        }
        if !watched_any {
            log::info!(
                "herdr config folder for {} doesn't exist; not watching it",
                path.display()
            );
        }
        Ok(Self { _watcher: watcher })
    }
}

/// What `herdr config check` said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckReport {
    /// "config: ok".
    pub ok: bool,
    /// The diagnostics after the first line, as herdr wrote them.
    pub details: String,
    /// herdr falls back to its defaults for the whole file.
    pub using_defaults: bool,
}

impl CheckReport {
    /// Reads `herdr config check`'s output.
    pub fn from_output(output: &str) -> Self {
        let mut lines = output.lines();
        let first = lines.next().unwrap_or_default().trim();
        let details = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
        Self {
            ok: first == "config: ok",
            using_defaults: details.contains("using defaults"),
            details,
        }
    }
}

/// Runs `bin config check` against the file at `path` (blocking; run it off
/// the UI thread).
pub fn check(bin: &Path, path: &Path) -> io::Result<CheckReport> {
    let output = Command::new(bin)
        .args(["config", "check"])
        .env(CONFIG_PATH_ENV, path)
        .output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CheckReport::from_output(&text))
}

#[cfg(test)]
mod tests;
