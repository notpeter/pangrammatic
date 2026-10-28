//! Types for the file upload API (`https://file-external.api.pangram.com`).
//!
//! This endpoint takes `multipart/form-data`, which progenitor does not
//! generate, so the request is built by hand in [`crate::Pangram::upload_files`].
//! The schema follows `spec/pangram-file-upload.json`.

use serde::{Deserialize, Serialize};

/// One result object per uploaded file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileUploadResult {
    /// Public dashboard URL for the uploaded file. Present when
    /// `public_dashboard_link` was `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_dashboard_link: Option<String>,
    /// Any additional, undocumented fields returned by the server.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A file to upload: a name as it appears in the multipart form, and bytes.
#[derive(Debug, Clone)]
pub struct UploadFile {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

impl UploadFile {
    /// Reads a file from disk, using its final path component as the name.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "upload".to_string());
        let bytes = std::fs::read(path)?;
        Ok(Self { file_name, bytes })
    }
}
