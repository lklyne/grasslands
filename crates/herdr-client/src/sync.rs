//! Keeps a [`State`] in step with a herdr server.
//!
//! One background thread owns the subscription and all reads. Events only mark
//! the cache stale; the thread then re-reads `session.snapshot` plus each tab's
//! `layout.export` and replaces the cache whole. On a dropped connection or
//! `events_lost` it resubscribes, re-reads and replaces the cache again, starting
//! the server first if nothing answers.
//!
//! herdr only scopes `pane.agent_status_changed` per pane, so the subscription
//! names every known pane and is reopened whenever the pane set changes. The new
//! subscription is acknowledged before the old one closes and before the next
//! read, so no change can fall between them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::client::Client;
use crate::endpoint::{self, Endpoint};
use crate::error::{Error, Result};
use crate::events;
use crate::state::State;
use crate::types::{LayoutDescription, TabId};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Bursts of events (a split emits several) are folded into one re-read.
const COALESCE_WINDOW: Duration = Duration::from_millis(10);
const READ_ATTEMPTS: usize = 4;
/// Most `layout.export` calls in flight at once. Over ssh each is ~0.2 s.
const LAYOUT_WORKERS: usize = 8;
const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    /// Lost the server or fell behind its events; retrying.
    Reconnecting {
        reason: String,
    },
    /// Not connected and not retrying, e.g. the endpoint was refused. The
    /// sync engine never reports this; apps use it when they don't start one.
    Disconnected {
        reason: String,
    },
}

/// Sent to the app on [`SessionSync::updates`].
#[derive(Debug, Clone)]
pub enum Update {
    /// The cache was replaced.
    State(Arc<State>),
    /// An event arrived (its kind, e.g. `pane_created`), before the re-read it
    /// triggers. The cache is the source of truth; this is for diagnostics.
    Event {
        kind: String,
    },
    Status(ConnectionStatus),
}

/// Handle to the background sync thread. Dropping it stops the thread.
#[derive(Debug)]
pub struct SessionSync {
    inbox: mpsc::Sender<Msg>,
    updates: async_channel::Receiver<Update>,
    client: Client,
}

impl SessionSync {
    /// Spawns the sync thread and returns immediately. The first state arrives as
    /// an [`Update::State`] with generation 1.
    pub fn start(endpoint: Endpoint) -> Result<Self> {
        let (inbox, inbox_rx) = mpsc::channel();
        let (updates_tx, updates) = async_channel::unbounded();
        let client = Client::new(endpoint.clone());
        let engine = Engine {
            endpoint,
            client: client.clone(),
            state: Arc::new(State::default()),
            status: ConnectionStatus::Connecting,
            inbox: inbox.clone(),
            inbox_rx,
            updates: updates_tx,
            sub: None,
            next_sub_gen: 0,
        };
        thread::Builder::new()
            .name("herdr-sync".into())
            .spawn(move || engine.run())?;
        Ok(Self {
            inbox,
            updates,
            client,
        })
    }

    /// Update stream. Works with `recv().await` on any executor (GPUI's included)
    /// or `recv_blocking()`. Each update goes to one receiver, so use one consumer.
    pub fn updates(&self) -> async_channel::Receiver<Update> {
        self.updates.clone()
    }

    /// Command handle on the same endpoint.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Asks for a re-read even without an event.
    pub fn refresh(&self) {
        let _ = self.inbox.send(Msg::Refresh);
    }
}

impl Drop for SessionSync {
    fn drop(&mut self) {
        let _ = self.inbox.send(Msg::Shutdown);
    }
}

enum Msg {
    Line { sub: u64, line: String },
    Closed { sub: u64 },
    Refresh,
    Shutdown,
}

struct Subscription {
    generation: u64,
    request_id: String,
    panes: BTreeSet<String>,
    stream: UnixStream,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // Unblocks the reader thread, which then exits.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Why the connected loop ended.
enum Stop {
    Shutdown,
    Lost(Error),
}

impl From<Error> for Stop {
    fn from(err: Error) -> Self {
        Self::Lost(err)
    }
}

struct Engine {
    endpoint: Endpoint,
    client: Client,
    /// The last published cache.
    state: Arc<State>,
    status: ConnectionStatus,
    inbox: mpsc::Sender<Msg>,
    inbox_rx: mpsc::Receiver<Msg>,
    updates: async_channel::Sender<Update>,
    sub: Option<Subscription>,
    next_sub_gen: u64,
}

impl Engine {
    fn run(mut self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            let stop = self.connected();
            if self.status == ConnectionStatus::Connected {
                backoff = BACKOFF_MIN;
            }
            let reason = match stop {
                Stop::Shutdown => return,
                Stop::Lost(err) => err.to_string(),
            };
            log::warn!("herdr sync lost: {reason}; retrying in {backoff:?}");
            self.sub = None;
            self.set_status(ConnectionStatus::Reconnecting { reason });
            if self.wait_or_shutdown(backoff) {
                return;
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Subscribes, reads, then serves events until something breaks.
    fn connected(&mut self) -> Stop {
        if let Err(err) = self
            .subscribe(self.known_panes())
            .and_then(|()| self.reread())
        {
            return Stop::Lost(err);
        }
        self.set_status(ConnectionStatus::Connected);
        loop {
            let Ok(msg) = self.inbox_rx.recv() else {
                return Stop::Shutdown;
            };
            let mut dirty = match self.handle(msg) {
                Ok(dirty) => dirty,
                Err(stop) => return stop,
            };
            loop {
                match self.inbox_rx.recv_timeout(COALESCE_WINDOW) {
                    Ok(msg) => match self.handle(msg) {
                        Ok(more) => dirty |= more,
                        Err(stop) => return stop,
                    },
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return Stop::Shutdown,
                }
            }
            if dirty && let Err(err) = self.reread() {
                return Stop::Lost(err);
            }
        }
    }

    /// Returns whether the cache is now stale.
    fn handle(&mut self, msg: Msg) -> Result<bool, Stop> {
        let current = self.sub.as_ref().map(|s| s.generation);
        match msg {
            Msg::Shutdown => Err(Stop::Shutdown),
            Msg::Refresh => Ok(true),
            Msg::Closed { sub } if Some(sub) == current => Err(Stop::Lost(Error::Closed)),
            Msg::Line { sub, line } if Some(sub) == current => {
                if let Some(kind) = events::kind(&line) {
                    let _ = self.updates.try_send(Update::Event { kind });
                    return Ok(true);
                }
                let request_id = self
                    .sub
                    .as_ref()
                    .map(|s| s.request_id.as_str())
                    .unwrap_or_default();
                match endpoint::parse_response(&line, request_id) {
                    // Includes `events_lost`, after which herdr closes the connection.
                    Err(err @ Error::Server { .. }) => Err(Stop::Lost(err)),
                    _ => {
                        log::debug!("ignoring subscription line: {line}");
                        Ok(false)
                    }
                }
            }
            // From a subscription we already replaced.
            Msg::Closed { .. } | Msg::Line { .. } => Ok(false),
        }
    }

    /// Opens a subscription for `panes`, waits for `subscription_started`, then
    /// replaces the old one.
    fn subscribe(&mut self, panes: BTreeSet<String>) -> Result<()> {
        let stream = self.endpoint.connect()?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let request_id = endpoint::next_id();
        let params =
            json!({ "subscriptions": events::subscriptions(panes.iter().map(String::as_str)) });
        endpoint::write_request(&stream, &request_id, "events.subscribe", &params)?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(Error::Closed);
        }
        endpoint::parse_response(&line, &request_id)?;
        stream.set_read_timeout(None)?;

        let generation = self.next_sub_gen;
        self.next_sub_gen += 1;
        let inbox = self.inbox.clone();
        thread::Builder::new()
            .name(format!("herdr-events-{generation}"))
            .spawn(move || read_events(reader, generation, &inbox))?;
        self.sub = Some(Subscription {
            generation,
            request_id,
            panes,
            stream,
        });
        Ok(())
    }

    /// Reads a fresh state and publishes it if it differs. Retries when the
    /// reads raced a change (tab trees disagree with the snapshot) or when the
    /// pane set moved and the subscription had to follow.
    fn reread(&mut self) -> Result<()> {
        let mut state = self.read_state()?;
        for attempt in 1..READ_ATTEMPTS {
            let panes: BTreeSet<String> = state.panes.keys().cloned().collect();
            let subscribed = self.sub.as_ref().map(|s| &s.panes);
            if subscribed != Some(&panes) {
                match self.subscribe(panes) {
                    Ok(()) => {}
                    // A pane vanished between the read and the subscribe.
                    Err(Error::Server { code, message }) if attempt + 1 < READ_ATTEMPTS => {
                        log::debug!("resubscribe rejected ({code}: {message}); re-reading");
                    }
                    Err(err) => return Err(err),
                }
            } else if state.inconsistent_tabs().is_empty() {
                break;
            }
            state = self.read_state()?;
        }
        if self
            .sub
            .as_ref()
            .is_some_and(|s| s.panes.iter().ne(state.panes.keys()))
        {
            // Still racing pane churn; settle on a later pass instead of spinning here.
            let _ = self.inbox.send(Msg::Refresh);
        }
        self.publish(state);
        Ok(())
    }

    fn read_state(&self) -> Result<State> {
        let snapshot = self.client.session_snapshot()?;
        let tab_ids: Vec<&TabId> = snapshot.tabs.iter().map(|tab| &tab.tab_id).collect();
        let layouts = export_layouts(&self.client, &tab_ids)?;
        Ok(State::build(0, snapshot, &layouts))
    }

    fn publish(&mut self, mut state: State) {
        state.generation = self.state.generation;
        if state == *self.state {
            return;
        }
        state.generation += 1;
        self.state = Arc::new(state);
        let _ = self
            .updates
            .try_send(Update::State(Arc::clone(&self.state)));
    }

    fn known_panes(&self) -> BTreeSet<String> {
        self.state.panes.keys().cloned().collect()
    }

    fn set_status(&mut self, status: ConnectionStatus) {
        if self.status != status {
            self.status = status.clone();
            let _ = self.updates.try_send(Update::Status(status));
        }
    }

    /// Sleeps for `delay`; returns true if asked to shut down meanwhile.
    fn wait_or_shutdown(&self, delay: Duration) -> bool {
        let deadline = std::time::Instant::now() + delay;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.inbox_rx.recv_timeout(left) {
                Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return true,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }
}

/// Exports every tab's layout, up to [`LAYOUT_WORKERS`] at a time. Tabs that
/// closed after the snapshot are skipped; the consistency retry in
/// [`Engine::reread`] settles them.
fn export_layouts(
    client: &Client,
    tab_ids: &[&TabId],
) -> Result<BTreeMap<TabId, LayoutDescription>> {
    let export = |tab_id: &TabId| match client.layout_export(Some(tab_id)) {
        Ok(layout) => Ok(Some(layout)),
        Err(Error::Server { code, .. }) if code == "not_found" || code == "tab_not_found" => {
            Ok(None)
        }
        Err(err) => Err(err),
    };
    let results: Vec<Result<Option<LayoutDescription>>> = if tab_ids.len() <= 1 {
        tab_ids.iter().map(|tab_id| export(tab_id)).collect()
    } else {
        let chunk = tab_ids.len().div_ceil(LAYOUT_WORKERS);
        thread::scope(|scope| {
            let workers: Vec<_> = tab_ids
                .chunks(chunk)
                .map(|tabs| {
                    scope.spawn(|| tabs.iter().map(|tab_id| export(tab_id)).collect::<Vec<_>>())
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        })
    };
    let mut layouts = BTreeMap::new();
    for (tab_id, result) in tab_ids.iter().zip(results) {
        if let Some(layout) = result? {
            layouts.insert((*tab_id).clone(), layout);
        }
    }
    Ok(layouts)
}

fn read_events(mut reader: BufReader<UnixStream>, sub: u64, inbox: &mpsc::Sender<Msg>) {
    let mut line = String::new();
    loop {
        line.clear();
        let msg = match reader.read_line(&mut line) {
            Ok(0) | Err(_) => Msg::Closed { sub },
            Ok(_) => Msg::Line {
                sub,
                line: line.trim_end().to_owned(),
            },
        };
        let closed = matches!(msg, Msg::Closed { .. });
        if inbox.send(msg).is_err() || closed {
            return;
        }
    }
}
