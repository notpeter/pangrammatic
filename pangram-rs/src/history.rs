//! A record of every HTTP exchange made through a [`crate::Pangram`] client.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One request/response pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// When the request completed, as an RFC 3339 UTC timestamp.
    pub timestamp: String,
    pub request_url: String,
    /// HTTP method, e.g. `GET` or `POST`.
    pub verb: String,
    /// The JSON request body, when the request had one. For multipart
    /// uploads this is a description of the form fields rather than the raw
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<serde_json::Value>,
    /// HTTP status code, absent when no response was received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The decoded response body. A non-JSON body is stored as a string;
    /// `null` when no response was received.
    pub response_body: serde_json::Value,
    pub response_headers: BTreeMap<String, String>,
    /// Set when the exchange failed without a usable HTTP response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An ordered list of [`Entry`] values.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct History {
    pub entries: Vec<Entry>,
}

impl History {
    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Writes the history as a pretty-printed JSON array.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(&self.entries)?;
        std::fs::write(path, json)
    }
}

pub(crate) fn headers_to_map(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let value = value.to_str().unwrap_or("<non-utf8>").to_string();
        out.entry(name.as_str().to_string())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    out
}

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
