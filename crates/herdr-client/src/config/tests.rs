use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::*;

fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        vars.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| OsString::from(v))
    }
}

#[test]
fn config_path_should_prefer_herdr_config_path() {
    let vars = [
        ("HERDR_CONFIG_PATH", "/x/custom.toml"),
        ("XDG_CONFIG_HOME", "/xdg"),
        ("HOME", "/home/u"),
    ];
    assert_eq!(
        config_path_with(env(&vars)),
        Some(PathBuf::from("/x/custom.toml"))
    );
}

#[test]
fn config_path_should_use_xdg_config_home_next() {
    let vars = [("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")];
    assert_eq!(
        config_path_with(env(&vars)),
        Some(PathBuf::from("/xdg/herdr/config.toml"))
    );
}

#[test]
fn config_path_should_fall_back_to_home_dot_config() {
    let vars = [("HOME", "/home/u")];
    assert_eq!(
        config_path_with(env(&vars)),
        Some(PathBuf::from("/home/u/.config/herdr/config.toml"))
    );
}

#[test]
fn config_path_should_treat_empty_variables_as_unset() {
    let vars = [
        ("HERDR_CONFIG_PATH", ""),
        ("XDG_CONFIG_HOME", ""),
        ("HOME", "/home/u"),
    ];
    assert_eq!(
        config_path_with(env(&vars)),
        Some(PathBuf::from("/home/u/.config/herdr/config.toml"))
    );
}

#[test]
fn config_path_should_be_none_without_any_variable() {
    assert_eq!(config_path_with(env(&[])), None);
}

#[test]
fn empty_config_should_give_herdr_defaults() {
    assert_eq!(parse("", None), Parsed::default());
}

#[test]
fn defaults_should_match_herdr_default_config() {
    let prefs = Prefs::default();
    assert_eq!(
        (
            prefs.sound.enabled,
            prefs.toast.delivery,
            prefs.toast.delay_seconds,
            prefs.agent_panel_sort,
            prefs.status_indicators,
            prefs.confirm_close,
            prefs.prompt_new_tab_name,
            prefs.prompt_new_workspace_name,
        ),
        (
            true,
            ToastDelivery::Off,
            1,
            AgentPanelSort::Spaces,
            StatusIndicators::Dots,
            true,
            true,
            false,
        )
    );
}

const ALL_SET: &str = r#"
[ui]
confirm_close = false
prompt_new_tab_name = false
prompt_new_workspace_name = true
agent_panel_sort = "priority"
status_indicators = "symbols"

[ui.toast]
delivery = "system"
delay_seconds = 0

[ui.sound]
enabled = false
path = "sounds/all.mp3"
done_path = "/abs/done.mp3"

[ui.sound.agents]
claude = "off"
droid = "on"
"#;

#[test]
fn every_known_key_should_be_read() {
    let prefs = parse(ALL_SET, Some(Path::new("/cfg"))).prefs;
    let expected = Prefs {
        sound: SoundPrefs {
            enabled: false,
            path: Some(PathBuf::from("/cfg/sounds/all.mp3")),
            done_path: Some(PathBuf::from("/abs/done.mp3")),
            request_path: None,
            agents: [
                ("claude".to_owned(), AgentSound::Off),
                ("droid".to_owned(), AgentSound::On),
            ]
            .into(),
        },
        toast: ToastPrefs {
            delivery: ToastDelivery::System,
            delay_seconds: 0,
        },
        agent_panel_sort: AgentPanelSort::Priority,
        status_indicators: StatusIndicators::Symbols,
        confirm_close: false,
        prompt_new_tab_name: false,
        prompt_new_workspace_name: true,
    };
    assert_eq!(prefs, expected);
}

#[test]
fn workspaces_should_be_an_alias_for_spaces() {
    let parsed = parse("[ui]\nagent_panel_sort = \"workspaces\"\n", None);
    assert_eq!(parsed.prefs.agent_panel_sort, AgentPanelSort::Spaces);
}

#[test]
fn inline_and_dotted_tables_should_be_read() {
    let text = "ui = { confirm_close = false }\n";
    assert!(!parse(text, None).prefs.confirm_close);
    let dotted = "ui.toast.delivery = \"herdr\"\n";
    assert_eq!(
        parse(dotted, None).prefs.toast.delivery,
        ToastDelivery::Herdr
    );
}

#[test]
fn a_bad_value_should_give_every_default_like_herdr() {
    let text = "[ui]\nconfirm_close = false\n[ui.toast]\ndelivery = \"loud\"\n";
    let parsed = parse(text, None);
    assert_eq!(
        (parsed.prefs, parsed.using_defaults),
        (Prefs::default(), true)
    );
}

#[test]
fn a_bad_value_should_be_reported() {
    let parsed = parse("[ui]\nconfirm_close = \"nope\"\n", None);
    assert_eq!(
        parsed.problems,
        ["invalid value for ui.confirm_close: expected true or false"]
    );
}

#[test]
fn a_fractional_delay_should_be_rejected_like_herdr() {
    let parsed = parse("[ui.toast]\ndelay_seconds = 2.5\n", None);
    assert!(parsed.using_defaults);
}

#[test]
fn a_negative_delay_should_be_rejected_like_herdr() {
    let parsed = parse("[ui.toast]\ndelay_seconds = -3\n", None);
    assert!(parsed.using_defaults);
}

#[test]
fn toml_that_does_not_parse_should_give_defaults() {
    let parsed = parse("ui = [\n", None);
    assert_eq!(
        (parsed.prefs, parsed.using_defaults),
        (Prefs::default(), true)
    );
}

#[test]
fn unknown_agent_keys_should_be_ignored_not_rejected() {
    let parsed = parse("[ui.sound.agents]\nopencode = \"off\"\nclaude = \"off\"\n", None);
    assert_eq!(
        (
            parsed.using_defaults,
            parsed.prefs.sound.agent("claude"),
            parsed.prefs.sound.agents.contains_key("opencode"),
        ),
        (false, AgentSound::Off, false)
    );
}

#[test]
fn unknown_keys_elsewhere_should_be_left_alone() {
    let parsed = parse("[ui]\nbogus = 1\nconfirm_close = false\n", None);
    assert!(!parsed.prefs.confirm_close);
}

#[test]
fn missing_file_should_give_defaults_without_problems() {
    let parsed = load(Path::new("/nonexistent/grasslands/herdr/config.toml"));
    assert_eq!(parsed, Parsed::default());
}

// Per-agent sound policy

#[test]
fn agents_should_sound_by_default() {
    assert!(SoundPrefs::default().plays_for(Some("claude")));
}

#[test]
fn droid_should_be_muted_by_default() {
    assert!(!SoundPrefs::default().plays_for(Some("droid")));
}

#[test]
fn on_should_unmute_droid() {
    let mut sound = SoundPrefs::default();
    sound.agents.insert("droid".into(), AgentSound::On);
    assert!(sound.plays_for(Some("droid")));
}

#[test]
fn off_should_mute_an_agent() {
    let mut sound = SoundPrefs::default();
    sound.agents.insert("claude".into(), AgentSound::Off);
    assert!(!sound.plays_for(Some("claude")));
}

#[test]
fn disabled_sound_should_mute_even_agents_set_on() {
    let mut sound = SoundPrefs {
        enabled: false,
        ..SoundPrefs::default()
    };
    sound.agents.insert("claude".into(), AgentSound::On);
    assert!(!sound.plays_for(Some("claude")));
}

#[test]
fn an_agent_of_unknown_kind_should_follow_enabled() {
    assert!(SoundPrefs::default().plays_for(None));
}

#[test]
fn custom_sound_should_prefer_the_specific_path() {
    let sound = SoundPrefs {
        path: Some("/all.mp3".into()),
        done_path: Some("/done.mp3".into()),
        ..SoundPrefs::default()
    };
    assert_eq!(
        (sound.custom_sound(true), sound.custom_sound(false)),
        (Some(Path::new("/done.mp3")), Some(Path::new("/all.mp3")))
    );
}

// Edits

const COMMENTED: &str = r#"# herdr configuration
# Place this file at ~/.config/herdr/config.toml

[ui]
# Ask for confirmation before closing a workspace
confirm_close = true   # keep this

# Background notification popup delivery
[ui.toast]
# off = disable pop-up notifications
# delivery = "off"
# delay_seconds = 1

[keys]
prefix = "ctrl+b" # my prefix
"#;

#[test]
fn replacing_a_value_should_keep_every_comment() {
    let edited = apply(COMMENTED, &Edit::ConfirmClose(false)).unwrap();
    assert_eq!(
        edited,
        COMMENTED.replace("confirm_close = true   #", "confirm_close = false   #")
    );
}

#[test]
fn adding_a_key_should_keep_every_comment() {
    let edited = apply(COMMENTED, &Edit::ToastDelivery(ToastDelivery::System)).unwrap();
    let comments = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| line.contains('#'))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(comments(&edited), comments(COMMENTED));
}

#[test]
fn an_added_key_should_read_back() {
    let edited = apply(COMMENTED, &Edit::ToastDelivery(ToastDelivery::System)).unwrap();
    assert_eq!(
        parse(&edited, None).prefs.toast.delivery,
        ToastDelivery::System
    );
}

#[test]
fn an_edit_should_add_missing_tables() {
    let edited = apply(
        "",
        &Edit::AgentSound {
            agent: "claude".into(),
            sound: AgentSound::Off,
        },
    )
    .unwrap();
    assert_eq!(edited, "[ui.sound.agents]\nclaude = \"off\"\n");
}

#[test]
fn setting_an_agent_back_to_default_should_remove_its_key() {
    let text = "[ui.sound.agents]\nclaude = \"off\"\ndroid = \"on\"\n";
    let edited = apply(
        text,
        &Edit::AgentSound {
            agent: "claude".into(),
            sound: AgentSound::Default,
        },
    )
    .unwrap();
    assert_eq!(edited, "[ui.sound.agents]\ndroid = \"on\"\n");
}

#[test]
fn removing_a_missing_key_should_change_nothing() {
    let edited = apply(
        COMMENTED,
        &Edit::AgentSound {
            agent: "claude".into(),
            sound: AgentSound::Default,
        },
    )
    .unwrap();
    assert_eq!(edited, COMMENTED);
}

#[test]
fn an_edit_should_write_into_an_inline_table() {
    let edited = apply("ui = { confirm_close = true }\n", &Edit::PromptNewTabName(false)).unwrap();
    assert_eq!(
        parse(&edited, None).prefs,
        Prefs {
            prompt_new_tab_name: false,
            ..Prefs::default()
        }
    );
}

#[test]
fn an_edit_should_refuse_a_file_that_does_not_parse() {
    assert!(matches!(
        apply("ui = [\n", &Edit::ConfirmClose(false)),
        Err(ConfigError::Parse(_))
    ));
}

#[test]
fn an_edit_should_refuse_a_value_in_a_tables_place() {
    assert!(matches!(
        apply("ui = 3\n", &Edit::ConfirmClose(false)),
        Err(ConfigError::NotATable(_))
    ));
}

#[test]
fn the_toast_delay_should_be_clamped_to_herdrs_range() {
    let edited = apply("", &Edit::ToastDelay(99_999)).unwrap();
    assert_eq!(parse(&edited, None).prefs.toast.delay_seconds, MAX_TOAST_DELAY);
}

#[test]
fn every_edit_should_match_apply_to() {
    let edits = [
        Edit::SoundEnabled(false),
        Edit::AgentSound {
            agent: "droid".into(),
            sound: AgentSound::On,
        },
        Edit::ToastDelivery(ToastDelivery::Terminal),
        Edit::ToastDelay(7),
        Edit::AgentPanelSort(AgentPanelSort::Priority),
        Edit::StatusIndicators(StatusIndicators::Symbols),
        Edit::ConfirmClose(false),
        Edit::PromptNewTabName(false),
        Edit::PromptNewWorkspaceName(true),
    ];
    let mut text = COMMENTED.to_owned();
    let mut expected = Prefs::default();
    for edit in &edits {
        text = apply(&text, edit).unwrap();
        edit.apply_to(&mut expected);
    }
    assert_eq!(parse(&text, None).prefs, expected);
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "grasslands-herdr-config-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn write_should_create_the_file_and_folder() {
    let dir = temp_dir("create");
    let path = dir.join("herdr/config.toml");
    write(&path, &Edit::ConfirmClose(false)).unwrap();
    assert!(!load(&path).prefs.confirm_close);
}

#[test]
fn write_should_keep_comments_on_disk() {
    let dir = temp_dir("comments");
    let path = dir.join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    write(&path, &Edit::ConfirmClose(false)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# herdr configuration\n") && text.contains("# my prefix"));
}

#[test]
fn write_should_edit_a_symlinks_target() {
    let dir = temp_dir("symlink");
    let target = dir.join("dotfiles.toml");
    std::fs::write(&target, COMMENTED).unwrap();
    let link = dir.join("config.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    write(&link, &Edit::ConfirmClose(false)).unwrap();
    let still_link = std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink();
    assert!(still_link && !load(&target).prefs.confirm_close);
}

#[test]
fn check_report_should_read_ok() {
    let report = CheckReport::from_output("config: ok\n");
    assert_eq!(
        report,
        CheckReport {
            ok: true,
            details: String::new(),
            using_defaults: false,
        }
    );
}

#[test]
fn check_report_should_see_herdr_falling_back_to_defaults() {
    let output = "config: issues found\nconfig parse error: TOML parse error at line 2\n  |\ninvalid type\n; using defaults\n";
    let report = CheckReport::from_output(output);
    assert!(!report.ok && report.using_defaults && report.details.starts_with("config parse error"));
}

#[test]
fn check_report_should_keep_warnings_without_defaults() {
    let output = "config: issues found\nunknown config key ui.sound.agents.opencode; ignoring key\n";
    assert!(!CheckReport::from_output(output).using_defaults);
}
