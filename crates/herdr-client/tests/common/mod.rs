//! Shared helpers for tests that drive the isolated `dev-*` herdr session.
//! Run them with `scripts/dev-run cargo test -p herdr-client`.

#![allow(dead_code)]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::{ConnectionStatus, Endpoint, SessionSync, State, Update};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The dev endpoint, or `None` (test skipped) unless the environment points at the
/// isolated dev session. Guards against ever touching the user's live herdr.
pub fn dev_endpoint() -> Option<Endpoint> {
    let Ok(endpoint) = Endpoint::from_env() else {
        eprintln!("skipping: herdr endpoint refused; run through scripts/dev-run");
        return None;
    };
    let isolated = endpoint
        .session
        .as_deref()
        .is_some_and(|s| s.starts_with("dev-"))
        && endpoint
            .socket_path
            .to_string_lossy()
            .contains("grasslands-herdr");
    if !isolated {
        eprintln!(
            "skipping: run through scripts/dev-run so herdr points at an isolated dev-* session"
        );
        return None;
    }
    Some(endpoint)
}

/// Follows a [`SessionSync`]'s updates on the test thread: the latest cache,
/// every status in order and every event kind seen.
pub struct Watch {
    updates: async_channel::Receiver<Update>,
    pub state: Arc<State>,
    pub statuses: Vec<ConnectionStatus>,
    pub events: Vec<String>,
}

impl Watch {
    /// Takes the sync's update stream; nothing else should read it.
    pub fn new(sync: &SessionSync) -> Self {
        Self {
            updates: sync.updates(),
            state: Arc::new(State::default()),
            statuses: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn status(&self) -> Option<&ConnectionStatus> {
        self.statuses.last()
    }

    fn drain(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::State(state) => self.state = state,
                Update::Status(status) => self.statuses.push(status),
                Update::Event { kind } => self.events.push(kind),
            }
        }
    }

    /// Drains updates until `pred` holds, panicking on timeout.
    pub fn wait(&mut self, what: &str, timeout: Duration, pred: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            if pred(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last state: {:#?}, statuses: {:?}",
                self.state,
                self.statuses
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the cache satisfies `pred` and returns it.
    pub fn wait_for(&mut self, what: &str, pred: impl Fn(&State) -> bool) -> Arc<State> {
        self.wait(what, TIMEOUT, |w| pred(&w.state));
        Arc::clone(&self.state)
    }
}
