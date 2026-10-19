//! Client for the herdr 0.9.5 socket API.
//!
//! - [`Endpoint`] finds the server from `HERDR_SOCKET_PATH` / `HERDR_SESSION` /
//!   `HERDR_BIN` and starts it when nothing answers.
//! - [`Client`] sends typed commands (one blocking round trip each).
//! - [`SessionSync`] runs a background thread that mirrors the session into an
//!   immutable [`State`] and pushes [`Update`]s on an `async_channel`, which GPUI's
//!   executor (or any other) can await.
//!
//! No async runtime is required: the crate uses plain threads and blocking Unix
//! sockets, so it works the same from GPUI, tokio or a test.

mod client;
mod endpoint;
mod error;
pub mod events;
mod machines;
mod remote_fs;
mod setup;
mod state;
mod sync;
pub mod types;

pub use client::*;
pub use endpoint::{ALLOW_LIVE_ENV, DEV_ROOT, Endpoint, check_dev_socket};
pub use error::{EVENTS_LOST, Error, Result};
pub use machines::{
    DEV_MACHINES_ENV, MachineLink, MachineProfile, MachineStatus, dev_allowed, list_machines,
    socket_dir,
};
pub use remote_fs::{ExecOutput, RemoteEntry, RemoteFs, RemoteMetadata};
pub use setup::{Check, Fix, FixCommand, InstallMethod, Installed, MIN_VERSION, Version};
pub use state::{SplitRef, SplitTree, State, Tab, Workspace};
pub use sync::{ConnectionStatus, SessionSync, Update};
pub use types::*;
