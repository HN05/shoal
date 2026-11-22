//! The repositories joined machines share through the hub, each by its
//! origin, which every machine matches its registrations by.
use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A repository's origin, as [`crate::repository::url_key`] reads the URL
/// it is cloned from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Origin(String);

impl Origin {
    /// The origin of the clone URL `url`.
    pub fn of(url: &str) -> Self {
        Self(crate::repository::url_key(url))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Origin {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> anyhow::Result<Self> {
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shared repository as the machine that last shared it registers it:
/// the URL other machines clone it from, and its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRepository {
    pub url: String,
    pub name: String,
}

/// A machine's edit of the shared repositories: one it shares, or, without
/// a value, one it stopped sharing or removed.
pub type Edit = super::sharing::Edit<Origin, SharedRepository>;

/// The hub's copy of a shared repository.
pub type Copy = super::sharing::Copy<Origin, SharedRepository>;
