//! Repository selectors and the names and identity a source path or URL
//! gives a repository.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::forge::repo::remote_url::{RemoteUrl, source_name};
use crate::repository_ids::RepositoryId;

/// What names a repository: the ID of a registered one, which selects it
/// alone, or the caller's argument, kept alongside a path resolved in its
/// directory, which may be an ID, name, path or source. The argument
/// serializes as before, and an ID as `{"id": ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Selector {
    #[serde(rename = "id")]
    Id(RepositoryId),
    #[serde(untagged)]
    Value(String),
    #[serde(untagged)]
    Path {
        value: String,
        path: std::path::PathBuf,
    },
}

impl Selector {
    pub fn value(&self) -> &str {
        match self {
            Self::Id(id) => id.as_str(),
            Self::Value(value) | Self::Path { value, .. } => value,
        }
    }

    /// The path or URL a registration of the selected repository reads.
    pub fn source(&self) -> Result<&str> {
        match self {
            Self::Id(id) => bail!("repository ID {id} is not a path or URL to register"),
            Self::Value(value) => Ok(value),
            Self::Path { path, .. } => path.to_str().context("repository path is not UTF-8"),
        }
    }

    pub fn path(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Value(value) if Path::new(value).is_absolute() => {
                std::fs::canonicalize(value).ok()
            }
            Self::Id(_) | Self::Value(_) => None,
            Self::Path { path, .. } => Some(path.clone()),
        }
    }
}

impl From<RepositoryId> for Selector {
    fn from(id: RepositoryId) -> Self {
        Self::Id(id)
    }
}

impl From<&RepositoryId> for Selector {
    fn from(id: &RepositoryId) -> Self {
        Self::Id(id.clone())
    }
}

impl From<String> for Selector {
    fn from(value: String) -> Self {
        Self::Value(value)
    }
}

impl From<&str> for Selector {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl From<&String> for Selector {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl From<&Selector> for Selector {
    fn from(value: &Selector) -> Self {
        value.clone()
    }
}

/// The explicit name, or the last path component of the source.
pub fn name(repo: &crate::model::Repository) -> &str {
    repo.name
        .as_deref()
        .unwrap_or_else(|| source_name(&repo.source))
}

/// A filesystem-safe clone directory name derived from the source.
pub fn directory_name(source: &str) -> String {
    let name: String = source_name(source)
        .chars()
        .take(crate::validate::MAX_NAME_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['.', '-', '_']);
    if name.is_empty() {
        "repository".into()
    } else {
        name.into()
    }
}

/// The host part of a remote URL (`https://user@host/...` or `user@host:...`),
/// without credentials.
pub fn host(source: &str) -> Option<&str> {
    RemoteUrl::parse(source).and_then(|remote| remote.host)
}

/// A remote URL with a host and repository path, as registration matches it.
pub fn is_remote_url(source: &str) -> bool {
    RemoteUrl::parse(source).is_some_and(|remote| remote.host.is_some() && remote.path.is_some())
}

/// Normalize equivalent transports of one remote to a comparable key.
pub fn url_key(url: &str) -> String {
    RemoteUrl::parse(url)
        .map(|remote| remote.registration_key())
        .unwrap_or_else(|| url.to_owned())
}

#[cfg(test)]
mod tests;
