//! The sync thread survives the dev server being stopped: it restarts the
//! server, resubscribes and replaces the cache.

mod common;

use std::process::Command;

use herdr_client::*;

use common::{dev_endpoint, wait_for};

#[test]
fn sync_should_reconnect_after_server_stop() {
    let Some(endpoint) = dev_endpoint() else {
        return;
    };
    let sync = SessionSync::start(endpoint.clone()).unwrap();
    let updates = sync.updates();
    wait_for(&sync, "first read", |s| s.generation > 0);
    assert_eq!(sync.status(), ConnectionStatus::Connected);

    // Only ever the isolated dev session (checked by dev_endpoint).
    let status = Command::new(&endpoint.bin)
        .args(["--session", "gpui-dev", "server", "stop"])
        .status()
        .unwrap();
    assert!(status.success());

    let mut saw_reconnecting = false;
    loop {
        let update = updates.recv_blocking().unwrap();
        match update {
            Update::Status(ConnectionStatus::Reconnecting { .. }) => saw_reconnecting = true,
            Update::Status(ConnectionStatus::Connected) if saw_reconnecting => break,
            _ => {}
        }
    }

    // The restarted server is live and the cache follows it again.
    let created = sync
        .client()
        .workspace_create(&NewWorkspace {
            label: Some("it-reconnect".into()),
            ..Default::default()
        })
        .unwrap();
    let ws = created.workspace.workspace_id;
    wait_for(&sync, "workspace after reconnect", |s| {
        s.workspace(&ws).is_some()
    });
    sync.client().workspace_close(&ws, false).unwrap();
}
