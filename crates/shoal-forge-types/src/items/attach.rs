//! Files attached to a description or comment, by path with their alt or
//! link text, and the limits on each kind of file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// The largest image Shoal uploads, which is GitHub's limit.
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// The largest video or other file Shoal uploads. Forges limit these by plan
/// or configuration, so this generous bound leaves the rest to them.
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// The content type of files whose extension [`TYPES`] does not name.
const OTHER_TYPE: &str = "application/octet-stream";

/// Extensions and their content types. The forge decides which files it
/// accepts; images and videos are shown, other files linked.
const TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("mp4", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("txt", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("patch", "text/x-diff"),
    ("json", "application/json"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tgz", "application/gzip"),
];

/// A file to attach, by absolute path, with an image's alt text or a
/// file's link text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
    #[serde(default)]
    pub alt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Image,
    Video,
    /// Any other file, which bodies link to.
    File,
}

impl Attachment {
    /// `<path>[#<alt text>]`, with the path relative to `cwd`.
    pub fn parse(arg: &str, cwd: &Path) -> Self {
        let (path, alt) = match arg.split_once('#') {
            Some((path, alt)) => (path, Some(alt.trim()).filter(|alt| !alt.is_empty())),
            None => (arg, None),
        };
        Self {
            path: cwd.join(path),
            alt: alt.map(str::to_owned),
        }
    }

    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment")
    }

    pub fn content_type(&self) -> &'static str {
        let extension = self
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        TYPES
            .iter()
            .find(|(known, _)| *known == extension)
            .map_or(OTHER_TYPE, |(_, content_type)| *content_type)
    }

    pub fn media(&self) -> Media {
        let content_type = self.content_type();
        if content_type.starts_with("image/") {
            Media::Image
        } else if content_type.starts_with("video/") {
            Media::Video
        } else {
            Media::File
        }
    }

    /// Refuse what no upload would accept: a relative path, a missing or
    /// empty file, or a file over its kind's limit.
    pub fn check(&self) -> Result<()> {
        let path = self.path.display();
        ensure!(self.path.is_absolute(), "{path}: give an absolute path");
        let metadata = std::fs::metadata(&self.path).with_context(|| format!("read {path}"))?;
        ensure!(metadata.is_file(), "{path} is not a file");
        ensure!(metadata.len() > 0, "{path} is empty");
        let (limit, kind) = match self.media() {
            Media::Image => (MAX_IMAGE_BYTES, "images"),
            Media::Video => (MAX_FILE_BYTES, "videos"),
            Media::File => (MAX_FILE_BYTES, "files"),
        };
        ensure!(
            metadata.len() <= limit,
            "{path}: {kind} must be at most {} MiB",
            limit / 1024 / 1024
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
