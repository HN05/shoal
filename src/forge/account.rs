//! Whose credentials a forge request uses: the user's own logins, or a
//! workspace's `[agent_auth]` account.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use super::api::{fj_keys_at, fj_keys_path, fj_token};
use crate::{agent_auth, git_profile, paths::Paths};

#[derive(Default)]
pub(crate) struct Account {
    /// The agent's `gh` wrapper and Git profile; the user's when absent.
    launch: Option<agent_auth::Launch>,
    /// The agent's saved fj logins; the user's when absent.
    fj_keys: Option<PathBuf>,
    /// The agent has an `fj` wrapper but no `fj_home`, so its Forgejo login
    /// cannot be found and the user's must not stand in for it.
    fj_home_missing: bool,
}

impl Account {
    /// `gh` as Shoal finds it and the token fj saved in its default location.
    pub fn user() -> Self {
        Self::default()
    }

    /// The workspace's agent account; options it leaves unset use the user's
    /// logins, as the agent itself would.
    pub fn agent(
        config: &agent_auth::Config,
        paths: &Paths,
        git: &git_profile::Git,
    ) -> Result<Self> {
        let launch = config.prepare(paths, git)?;
        let fj_keys = config
            .fj_home
            .as_ref()
            .map(|home| fj_keys_at(&crate::fsutil::expand_home(home, &paths.home), None));
        Ok(Self {
            launch: Some(launch),
            fj_home_missing: config.fj.is_some() && fj_keys.is_none(),
            fj_keys,
        })
    }

    /// Select this account's `gh` for a request.
    pub(super) fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(launch) = &self.launch {
            launch.apply(command);
        }
    }

    /// The token for `host`. The user may have none, which leaves an anonymous
    /// request that reaches only public repositories; the agent must have one.
    pub(super) fn forgejo_token(&self, host: &str) -> Result<Option<String>> {
        ensure!(
            !self.fj_home_missing,
            "set agent_auth.fj_home to the HOME the agent's fj wrapper uses, so Shoal can use the agent's Forgejo login"
        );
        match &self.fj_keys {
            Some(path) => fj_token(path, host)
                .map(Some)
                .context("the agent's fj login"),
            None => Ok(fj_token(&fj_keys_path()?, host).ok()),
        }
    }
}
