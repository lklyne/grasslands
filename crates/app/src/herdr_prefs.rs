//! herdr's own preferences (its `config.toml`) in the app. herdr stays the
//! source of truth: the app reads the file herdr reads, follows it live (a
//! file watcher, and a reread when the main window activates), and the
//! Settings window's herdr page writes changes back into that file, keeping
//! the user's comments, then asks Local's herdr to `server.reload_config`.
//! Remote machines keep their own config on the remote; only Local's is here.
//! See docs/decisions/0022-herdr-prefs.md.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::notification::NotificationDelivery;
use gpui_kit::*;

use crate::herdr::config::{
    self, ALLOW_LIVE_ENV, CheckReport, ConfigReloadStatus, ConfigWatcher, DEV_ROOT, Edit, Prefs,
    ToastDelivery,
};
use crate::herdr::{Agent, Snapshot};
use crate::state::{AppState, MachineId};

/// How long the watcher waits for an editor's burst of writes to settle.
const SETTLE: Duration = Duration::from_millis(150);

/// What the app knows about herdr's config.
#[derive(Default)]
pub struct HerdrPrefs {
    /// What herdr runs with.
    prefs: Prefs,
    /// The file herdr reads; `None` without `HOME`.
    path: Option<PathBuf>,
    /// What reading the file found wrong.
    problems: Vec<String>,
    /// `herdr config check`'s last answer.
    check: Option<CheckReport>,
    /// What went wrong writing or reloading, last time.
    write_error: Option<String>,
    /// `server.reload_config`'s diagnostics when it didn't fully apply.
    reload_diagnostics: Vec<String>,
    state: Option<WeakEntity<AppState>>,
    /// Edits a script run keeps applied over whatever the file says, so a
    /// dialog herdr's config asks for doesn't stall it (never written).
    script_overrides: Vec<Edit>,
    /// The file's modification time and length when it was last checked.
    checked: Option<(SystemTime, u64)>,
    _watcher: Option<ConfigWatcher>,
    _watch: Option<Task<()>>,
}

impl Global for HerdrPrefs {}

impl HerdrPrefs {
    fn apply_overrides(&self, prefs: &mut Prefs) {
        for edit in &self.script_overrides {
            edit.apply_to(prefs);
        }
    }
}

/// Keeps `edit` applied for the rest of a script run, whatever the file
/// says, without writing it (debug builds' scripts only).
#[cfg(debug_assertions)]
pub fn override_for_script(edit: Edit, cx: &mut App) {
    let global = cx.default_global::<HerdrPrefs>();
    edit.apply_to(&mut global.prefs);
    cx.refresh_windows();
    let global = cx.default_global::<HerdrPrefs>();
    global.script_overrides.push(edit);
}

/// Reads the config and starts watching it.
pub fn init(cx: &mut App) {
    let path = config::config_path();
    let mut prefs = HerdrPrefs {
        path: path.clone(),
        ..HerdrPrefs::default()
    };
    if let Some(path) = &path {
        let parsed = config::load(path);
        log_problems(path, &parsed.problems);
        prefs.prefs = parsed.prefs;
        prefs.problems = parsed.problems;
        let (changes, changed) = async_channel::unbounded();
        match ConfigWatcher::new(path, changes) {
            Ok(watcher) => {
                prefs._watcher = Some(watcher);
                prefs._watch = Some(cx.spawn(async move |cx| {
                    while changed.recv().await.is_ok() {
                        cx.background_executor().timer(SETTLE).await;
                        while changed.try_recv().is_ok() {}
                        cx.update(reload);
                    }
                }));
            }
            Err(error) => tracing::warn!("herdr config: can't watch {}: {error}", path.display()),
        }
    }
    cx.set_global(prefs);
}

/// Gives the prefs Local's herdr, for `config check` and `reload_config`.
pub fn attach(state: &Entity<AppState>, cx: &mut App) {
    cx.default_global::<HerdrPrefs>().state = Some(state.downgrade());
    run_check(cx);
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

fn log_problems(path: &Path, problems: &[String]) {
    for problem in problems {
        tracing::warn!("herdr config {}: {problem}", path.display());
    }
}

/// The preferences herdr runs with (herdr's defaults before `init`).
pub fn get(cx: &App) -> &Prefs {
    static DEFAULTS: std::sync::OnceLock<Prefs> = std::sync::OnceLock::new();
    cx.try_global::<HerdrPrefs>()
        .map_or_else(|| DEFAULTS.get_or_init(Prefs::default), |p| &p.prefs)
}

/// The config file's path.
pub fn path(cx: &App) -> Option<&Path> {
    cx.try_global::<HerdrPrefs>()?.path.as_deref()
}

/// The kinds of agent (`claude`, `codex`, …) running on Local now.
pub fn running_agents(cx: &App) -> BTreeSet<String> {
    let Some(state) = cx
        .try_global::<HerdrPrefs>()
        .and_then(|p| p.state.as_ref()?.upgrade())
    else {
        return BTreeSet::new();
    };
    let state = state.read(cx);
    state
        .machine(&MachineId::Local)
        .map(|local| {
            local
                .snapshot()
                .agents
                .iter()
                .filter_map(|agent| agent.agent.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Everything wrong with the config right now, one entry each: reading it,
/// `herdr config check`, the last write and the last reload.
pub fn problems(cx: &App) -> Vec<String> {
    let Some(prefs) = cx.try_global::<HerdrPrefs>() else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    match &prefs.check {
        // herdr's own words beat ours when it ran.
        Some(check) if !check.ok => problems.push(check.details.clone()),
        Some(_) => {}
        None => problems.extend(prefs.problems.iter().cloned()),
    }
    problems.extend(prefs.write_error.iter().cloned());
    problems.extend(prefs.reload_diagnostics.iter().cloned());
    problems
}

/// Rereads the file (on a change, or when the window activates), then has
/// herdr check it.
pub fn reload(cx: &mut App) {
    let Some(path) = path(cx).map(Path::to_path_buf) else {
        return;
    };
    let parsed = config::load(&path);
    let global = cx.default_global::<HerdrPrefs>();
    let mut prefs = parsed.prefs;
    // herdr already said it ignores this very file: keep its defaults.
    let rejected = global.check.as_ref().is_some_and(|c| c.using_defaults);
    if rejected && global.checked.is_some() && global.checked == file_stamp(&path) {
        prefs = Prefs::default();
    }
    global.apply_overrides(&mut prefs);
    if global.prefs != prefs || global.problems != parsed.problems {
        log_problems(&path, &parsed.problems);
        tracing::info!("herdr config: reread {}", path.display());
        global.prefs = prefs;
        global.problems = parsed.problems;
        // Glyphs, the agents order and the dialogs read the prefs in render.
        cx.refresh_windows();
    }
    run_check(cx);
}

/// Runs `herdr config check` off the main thread. When herdr says it falls
/// back to its defaults for the whole file, so does the app.
fn run_check(cx: &mut App) {
    let Some(path) = path(cx).map(Path::to_path_buf) else {
        return;
    };
    let bin = cx
        .try_global::<HerdrPrefs>()
        .and_then(|p| p.state.as_ref()?.upgrade())
        .and_then(|state| Some(state.read(cx).herdr_config.as_ref()?.bin.clone()));
    let Some(bin) = bin else { return };
    // Window activations reread often; ask herdr again only when the file
    // changed (or a write or reload since may have).
    let stamp = file_stamp(&path);
    let global = cx.default_global::<HerdrPrefs>();
    if global.check.is_some() && global.checked == stamp {
        return;
    }
    global.checked = stamp;
    let task = cx
        .background_executor()
        .spawn(async move { config::check(&bin, &path) });
    cx.spawn(async move |cx| {
        let report = match task.await {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!("herdr config check: {error}");
                return;
            }
        };
        cx.update(|cx| {
            let global = cx.default_global::<HerdrPrefs>();
            if report.using_defaults {
                let mut prefs = Prefs::default();
                global.apply_overrides(&mut prefs);
                global.prefs = prefs;
            }
            if global.check.as_ref() != Some(&report) {
                if !report.ok {
                    tracing::warn!("herdr config check: {}", report.details);
                }
                global.check = Some(report);
            }
        });
    })
    .detach();
}

/// Whether this run may write herdr's config. Tests never do; script runs
/// only in the `herdr-prefs` scenario; debug builds only under the dev root
/// (`scripts/dev-run`), unless `RT_ALLOW_LIVE_HERDR=1`.
fn writable(path: &Path) -> Result<(), String> {
    if cfg!(test) {
        return Err("tests don't write herdr's config".into());
    }
    if cfg!(debug_assertions)
        && std::env::var("RT_SCRIPT").is_ok_and(|s| !s.is_empty() && s != "herdr-prefs")
    {
        return Err("script runs don't write herdr's config".into());
    }
    if cfg!(debug_assertions) && std::env::var(ALLOW_LIVE_ENV).is_err() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let dev =
            home.is_some_and(|home| path.is_absolute() && path.starts_with(home.join(DEV_ROOT)));
        if !dev {
            return Err(format!(
                "debug builds only write herdr configs under ~/{DEV_ROOT}; {} is not (set {ALLOW_LIVE_ENV}=1 to override)",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Changes one herdr preference: at once in the app, then in herdr's
/// `config.toml` (comments and formatting kept), then in Local's herdr
/// through `server.reload_config`.
pub fn set(edit: Edit, cx: &mut App) {
    edit.apply_to(&mut cx.default_global::<HerdrPrefs>().prefs);
    cx.refresh_windows();
    let global = cx.default_global::<HerdrPrefs>();
    let Some(path) = global.path.clone() else {
        global.write_error = Some("no herdr config path: HOME is not set".into());
        return;
    };
    if let Err(why) = writable(&path) {
        tracing::info!("herdr config: not written ({why}): {edit:?}");
        return;
    }
    // On this thread, so quick edits land one after another and none
    // overwrites another's read of the file; it's one small file.
    match config::write(&path, &edit) {
        Ok(()) => {
            tracing::info!("herdr config: wrote {edit:?} to {}", path.display());
            cx.default_global::<HerdrPrefs>().write_error = None;
            reload_server(cx);
        }
        Err(error) => {
            tracing::warn!("herdr config: {error}");
            cx.default_global::<HerdrPrefs>().write_error = Some(error.to_string());
            // Show what the file really says again.
            reload(cx);
        }
    }
}

/// Asks Local's herdr to reread its config (`server.reload_config`).
pub fn reload_server(cx: &mut App) {
    let Some(state) = cx
        .try_global::<HerdrPrefs>()
        .and_then(|p| p.state.as_ref()?.upgrade())
    else {
        return;
    };
    let task = state
        .read(cx)
        .call(&MachineId::Local, cx, |c| c.server_reload_config());
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            let global = cx.default_global::<HerdrPrefs>();
            global.reload_diagnostics = match result {
                Ok(reload) if reload.status == ConfigReloadStatus::Applied => Vec::new(),
                Ok(reload) => {
                    let mut lines = vec![format!(
                        "herdr's reload_config: {}",
                        match reload.status {
                            ConfigReloadStatus::Partial => "partly applied",
                            _ => "failed",
                        }
                    )];
                    lines.extend(reload.diagnostics);
                    lines
                }
                Err(error) => vec![format!("herdr's reload_config: {error}")],
            };
            reload(cx);
        });
    })
    .detach();
}

/// Whether the user is looking at an agent right now: the main window is
/// active, its machine is the selected one, and it is in the focused
/// workspace (`workspace`) or the focused tab (`tab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Looking {
    pub workspace: bool,
    pub tab: bool,
}

impl Looking {
    pub fn at(agent: &Agent, snapshot: &Snapshot, selected: bool, window_active: bool) -> Self {
        let shown = window_active && selected;
        let workspace =
            shown && snapshot.focused_workspace_id.as_deref() == Some(agent.workspace_id.as_str());
        Self {
            workspace,
            tab: workspace && snapshot.focused_tab_id.as_deref() == Some(agent.tab_id.as_str()),
        }
    }
}

/// What a finished or blocked agent gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub sound: bool,
    /// `None`: no toast.
    pub toast: Option<NotificationDelivery>,
}

/// herdr's policy for an agent of kind `agent` reaching blocked or done:
/// sounds only for background workspaces (`[ui.sound]`, per-agent overrides,
/// `HERDR_DISABLE_SOUND`), popups never for the active tab, sent the way
/// `ui.toast.delivery` says. An in-app toast is what grasslands has for
/// herdr's own toast and the outer terminal's; while the window is in the
/// background, `terminal` adds an OS notification and `system` sends only
/// that. See decision 0022.
pub fn alert(
    prefs: &Prefs,
    agent: Option<&str>,
    looking: Looking,
    window_active: bool,
    sound_disabled_by_env: bool,
) -> Alert {
    let sound = !looking.workspace && !sound_disabled_by_env && prefs.sound.plays_for(agent);
    let toast = match prefs.toast.delivery {
        _ if looking.tab => None,
        ToastDelivery::Off => None,
        ToastDelivery::Herdr => Some(NotificationDelivery::InApp),
        ToastDelivery::Terminal if window_active => Some(NotificationDelivery::InApp),
        ToastDelivery::Terminal => Some(NotificationDelivery::InAppAndSystem),
        ToastDelivery::System if window_active => Some(NotificationDelivery::InApp),
        ToastDelivery::System => Some(NotificationDelivery::System),
    };
    Alert { sound, toast }
}

#[cfg(test)]
mod tests {
    use super::{Alert, Looking, alert};
    use crate::herdr::config::{AgentSound, Prefs, ToastDelivery};
    use gpui_kit::component::notification::NotificationDelivery;

    const AWAY: Looking = Looking {
        workspace: false,
        tab: false,
    };
    const SAME_WORKSPACE: Looking = Looking {
        workspace: true,
        tab: false,
    };
    const SAME_TAB: Looking = Looking {
        workspace: true,
        tab: true,
    };

    fn with_delivery(delivery: ToastDelivery) -> Prefs {
        let mut prefs = Prefs::default();
        prefs.toast.delivery = delivery;
        prefs
    }

    #[test]
    fn a_background_workspace_should_get_a_sound() {
        assert!(alert(&Prefs::default(), Some("claude"), AWAY, true, false).sound);
    }

    #[test]
    fn the_workspace_in_view_should_get_no_sound() {
        assert!(
            !alert(
                &Prefs::default(),
                Some("claude"),
                SAME_WORKSPACE,
                true,
                false
            )
            .sound
        );
    }

    #[test]
    fn herdr_disable_sound_should_silence_agents() {
        assert!(!alert(&Prefs::default(), Some("claude"), AWAY, true, true).sound);
    }

    #[test]
    fn droid_should_be_silent_by_default() {
        assert!(!alert(&Prefs::default(), Some("droid"), AWAY, true, false).sound);
    }

    #[test]
    fn a_muted_agent_should_be_silent() {
        let mut prefs = Prefs::default();
        prefs.sound.agents.insert("codex".into(), AgentSound::Off);
        assert!(!alert(&prefs, Some("codex"), AWAY, true, false).sound);
    }

    #[test]
    fn delivery_off_should_mean_no_toast() {
        let prefs = with_delivery(ToastDelivery::Off);
        assert_eq!(alert(&prefs, None, AWAY, true, false).toast, None);
    }

    #[test]
    fn the_active_tab_should_get_no_toast() {
        let prefs = with_delivery(ToastDelivery::Herdr);
        assert_eq!(alert(&prefs, None, SAME_TAB, true, false).toast, None);
    }

    #[test]
    fn another_tab_of_the_workspace_in_view_should_get_a_toast_but_no_sound() {
        let prefs = with_delivery(ToastDelivery::Herdr);
        assert_eq!(
            alert(&prefs, Some("claude"), SAME_WORKSPACE, true, false),
            Alert {
                sound: false,
                toast: Some(NotificationDelivery::InApp),
            }
        );
    }

    #[test]
    fn terminal_delivery_should_add_the_os_in_the_background() {
        let prefs = with_delivery(ToastDelivery::Terminal);
        assert_eq!(
            alert(&prefs, None, AWAY, false, false).toast,
            Some(NotificationDelivery::InAppAndSystem)
        );
    }

    #[test]
    fn system_delivery_should_go_to_the_os_in_the_background() {
        let prefs = with_delivery(ToastDelivery::System);
        assert_eq!(
            alert(&prefs, None, AWAY, false, false).toast,
            Some(NotificationDelivery::System)
        );
    }

    #[test]
    fn system_delivery_should_toast_in_app_while_the_window_is_active() {
        let prefs = with_delivery(ToastDelivery::System);
        assert_eq!(
            alert(&prefs, None, AWAY, true, false).toast,
            Some(NotificationDelivery::InApp)
        );
    }
}
