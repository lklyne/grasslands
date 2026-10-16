use std::io;

/// Error code herdr sends on a subscription when the subscriber fell behind
/// the server's retained event history. The connection is closed after it.
pub const EVENTS_LOST: &str = "events_lost";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("herdr socket: {0}")]
    Io(#[from] io::Error),
    #[error("herdr json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("decoding {method} result: {source}")]
    Decode {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("herdr error {code}: {message}")]
    Server { code: String, message: String },
    #[error("herdr protocol: {0}")]
    Protocol(String),
    #[error("starting herdr server: {0}")]
    ServerStart(String),
    #[error("herdr closed the connection")]
    Closed,
}

impl Error {
    /// The server's error code, if this is an error response.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Server { code, .. } => Some(code),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
