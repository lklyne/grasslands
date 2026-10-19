//! The sync thread survives the dev server being stopped: it restarts the
//! server, resubscribes and replaces the cache.

mod common;

use std::process::Command;

use herdr_client::*;

use common::{Watch, dev_endpoint};

#[test]
fn sync_should_reconnect_after_server_stop() {
    let Some(endpoint) = dev_endpoint() else {
        return;
    };
    let sync = SessionSync::start(endpoint.clone()).unwrap();
    let mut watch = Watch::new(&sync);
    watch.wait_for("first read", |s| s.generation > 0);
    assert_eq!(watch.status(), Some(&ConnectionStatus::Connected));

    // Only ever the isolated dev session (checked by dev_endpoint).
    let status = Command::new(&endpoint.bin)
        .args([
            "--session",
            endpoint.session.as_deref().unwrap(),
            "server",
            "stop",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let seen = watch.statuses.len();
    watch.wait("reconnect", common::TIMEOUT, |w| {
        let after = &w.statuses[seen..];
        after
            .iter()
            .position(|s| matches!(s, ConnectionStatus::Reconnecting { .. }))
            .is_some_and(|i| after[i..].contains(&ConnectionStatus::Connected))
    });

    // The restarted server is live and the cache follows it again.
    let created = sync
        .client()
        .workspace_create(&NewWorkspace {
            label: Some("it-reconnect".into()),
            ..Default::default()
        })
        .unwrap();
    let ws = created.workspace.workspace_id;
    watch.wait_for("workspace after reconnect", |s| s.workspace(&ws).is_some());
    sync.client().workspace_close(&ws, false).unwrap();
}
