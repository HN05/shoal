//! Apps built on Shoal: their `[apps.<name>]` settings, which Shoal hands
//! them unread, and their app store entries.
use std::{fmt, str::FromStr};

use anyhow::{Result, ensure};
use shoal_states::text_key;

pub mod store;

/// The name an app goes by in Shoal: its `[apps.<name>]` table's. At most
/// [`AppName::MAX`] bytes of printable text without a `/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppName(String);

impl AppName {
    pub const MAX: usize = 64;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AppName {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(
            !text.is_empty() && text.len() <= Self::MAX,
            "an app's name takes 1 to {} bytes",
            Self::MAX
        );
        ensure!(
            !text.contains(|c: char| c.is_control() || c == '/'),
            "an app's name has no control characters or /: {text:?}"
        );
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for AppName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(AppName);

#[cfg(test)]
mod tests;
