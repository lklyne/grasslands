//! Shared helpers for tests that drive the isolated `gpui-dev` herdr session.
//! Run them with `scripts/dev-run cargo test -p herdr-client`.

#![allow(dead_code)]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::{Endpoint, SessionSync, State};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The dev endpoint, or `None` (test skipped) unless the environment points at the
/// isolated dev session. Guards against ever touching the user's live herdr.
pub fn dev_endpoint() -> Option<Endpoint> {
    let endpoint = Endpoint::from_env();
    let isolated = endpoint.session.as_deref() == Some("gpui-dev")
        && endpoint
            .socket_path
            .to_string_lossy()
            .contains("rusted-tools-herdr");
    if !isolated {
        eprintln!("skipping: run through scripts/dev-run so herdr points at the gpui-dev session");
        return None;
    }
    Some(endpoint)
}

/// Polls the cache until `pred` holds, panicking with the last state on timeout.
pub fn wait_for(sync: &SessionSync, what: &str, pred: impl Fn(&State) -> bool) -> Arc<State> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = sync.state();
        if pred(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; last state: {state:#?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
