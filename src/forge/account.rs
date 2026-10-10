//! Whose credentials a forge request uses.
use anyhow::Result;

use super::api::{fj_keys_path, fj_token};

/// The user's own logins: `gh` as Shoal finds it and the token fj saved in
/// its default location.
#[derive(Default)]
pub(crate) struct Account {}

impl Account {
    pub fn user() -> Self {
        Self::default()
    }

    /// The token for `host`, or none for an anonymous request, which reaches
    /// only public repositories.
    pub(super) fn forgejo_token(&self, host: &str) -> Result<Option<String>> {
        Ok(fj_token(&fj_keys_path()?, host).ok())
    }
}
