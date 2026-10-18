//! Against a scripted fake herdr: `events_lost` on the subscription makes the
//! sync thread resubscribe, re-read and replace the cache.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use herdr_client::*;
use serde_json::{Value, json};

use common::wait_for;

struct Fake {
    subscriptions: AtomicUsize,
    snapshots: AtomicUsize,
}

fn snapshot(label: &str) -> Value {
    json!({
        "version": "0.9.5", "protocol": 22,
        "workspaces": [{"workspace_id": "w1", "number": 1, "label": label, "focused": true,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "w1:t1", "agent_status": "idle"}],
        "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1",
            "focused": true, "pane_count": 1, "agent_status": "idle"}],
        "panes": [{"pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
            "tab_id": "w1:t1", "focused": true, "agent_status": "idle", "revision": 0}],
        "layouts": [], "agents": []
    })
}

fn serve(fake: Arc<Fake>, stream: UnixStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let req: Value = serde_json::from_str(&line).unwrap();
    let id = req["id"].clone();
    let mut send = |v: Value| {
        let _ = writeln!(writer, "{v}");
    };
    match req["method"].as_str().unwrap() {
        "events.subscribe" => {
            let n = fake.subscriptions.fetch_add(1, Ordering::SeqCst);
            send(json!({"id": id, "result": {"type": "subscription_started"}}));
            // The first subscription has no panes yet and is replaced as soon as the
            // client learns w1:p1, so lose events on the second one.
            if n == 1 {
                thread::sleep(std::time::Duration::from_millis(100));
                send(
                    json!({"id": id, "error": {"code": "events_lost", "message": "subscriber fell behind"}}),
                );
                return;
            }
            // Later subscriptions stay open until the client hangs up.
            let _ = reader.read_line(&mut line);
        }
        "session.snapshot" => {
            fake.snapshots.fetch_add(1, Ordering::SeqCst);
            // Label each snapshot with the subscription it was read under.
            let label = format!("sub{}", fake.subscriptions.load(Ordering::SeqCst));
            send(
                json!({"id": id, "result": {"type": "session_snapshot", "snapshot": snapshot(&label)}}),
            );
        }
        "layout.export" => send(
            json!({"id": id, "result": {"type": "layout_export", "layout": {
            "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false, "focused_pane_id": "w1:p1",
            "root": {"type": "pane", "pane_id": "w1:p1"}}}}),
        ),
        other => send(json!({"id": id, "error": {"code": "unsupported", "message": other}})),
    }
}

#[test]
fn events_lost_should_trigger_resubscribe_and_fresh_snapshot() {
    let dir = std::env::temp_dir().join(format!("herdr-fake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path: PathBuf = dir.join("herdr.sock");
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();
    let fake = Arc::new(Fake {
        subscriptions: AtomicUsize::new(0),
        snapshots: AtomicUsize::new(0),
    });
    let server = Arc::clone(&fake);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let fake = Arc::clone(&server);
            thread::spawn(move || serve(fake, stream));
        }
    });

    let endpoint = Endpoint {
        bin: "false".into(),
        session: None,
        socket_path,
        auto_start: false,
        server_log: None,
    };
    let sync = SessionSync::start(endpoint).unwrap();
    let updates = sync.updates();

    let reason = loop {
        if let Update::Status(ConnectionStatus::Reconnecting { reason }) =
            updates.recv_blocking().unwrap()
        {
            break reason;
        }
    };
    assert!(reason.contains(EVENTS_LOST), "{reason}");
    let state = wait_for(&sync, "re-read after events_lost", |s| {
        s.workspace("w1").is_some_and(|w| w.info.label == "sub3")
    });
    assert_eq!(fake.subscriptions.load(Ordering::SeqCst), 3);
    assert_eq!(sync.status(), ConnectionStatus::Connected);
    assert_eq!(
        state.tab("w1:t1").and_then(|t| t.root.clone()),
        Some(SplitTree::Pane("w1:p1".into()))
    );
    let _ = std::fs::remove_dir_all(dir);
}
