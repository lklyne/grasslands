//! Shared by the live link tests.

use std::env;
use std::path::PathBuf;
use std::sync::Once;

use herdr_client::MachineProfile;

static SAVED: Once = Once::new();

/// The local herdr: `HERDR_BIN` (set by scripts/dev-run), else `herdr`.
pub fn herdr_bin() -> PathBuf {
    env::var_os("HERDR_BIN")
        .filter(|bin| !bin.is_empty())
        .map_or_else(|| PathBuf::from("herdr"), PathBuf::from)
}

/// Makes the herdr CLI of this process (and its children) know exactly
/// `profiles`, so `herdr machine status` can check them, without touching
/// the saved machines the dev sessions share with the user's herdr: herdr
/// keeps them under `$XDG_STATE_HOME/herdr/client/endpoints.json`, and this
/// points `XDG_STATE_HOME` at a private directory, one per test binary
/// (reused by its later runs and by the child processes it starts). Once
/// per process, so pass every profile the test binary uses.
pub fn herdr_knows(profiles: &[MachineProfile]) {
    SAVED.call_once(|| {
        let binary = env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.file_stem()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "tests".to_owned());
        let dir = env::temp_dir().join(format!("grasslands-it-state-{binary}"));
        let client = dir.join("herdr/client");
        std::fs::create_dir_all(&client).unwrap();
        let ssh: Vec<_> = profiles
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id, "label": p.label, "target": p.target,
                    "session": p.session, "enabled": p.enabled,
                })
            })
            .collect();
        let saved = serde_json::json!({ "version": 1, "ssh": ssh });
        std::fs::write(client.join("endpoints.json"), saved.to_string()).unwrap();
        // SAFETY: set before any link starts a thread that reads it.
        unsafe { env::set_var("XDG_STATE_HOME", &dir) };
    });
}
