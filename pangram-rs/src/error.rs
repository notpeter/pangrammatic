use std::collections::BTreeMap;
use std::time::Duration;

/// Errors produced by the [`crate::Pangram`] client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered with a non-success HTTP status.
    #[error("{verb} {url} returned HTTP {status}: {body}")]
    Api {
        verb: String,
        url: String,
        status: u16,
        headers: BTreeMap<String, String>,
        /// The decoded JSON body, or a JSON string holding the raw body when
        /// it was not JSON.
        body: serde_json::Value,
    },

    /// The request could not be sent or the connection failed.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The request was rejected before being sent.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// A success response whose body could not be decoded.
    #[error("could not decode response from {url}: {message}")]
    InvalidResponse { url: String, message: String },

    /// Polling exceeded the configured timeout.
    #[error("timed out after {0:?} waiting for {1}")]
    Timeout(Duration, String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
