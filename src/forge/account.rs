//! Whose credentials a forge request uses: the user's own logins, or a
//! workspace's `[agent_auth]` account.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use tokio::sync::OnceCell;

use super::{
    api::{fj_keys_at, fj_keys_path, fj_token},
    bitbucket::Credentials,
    tea::{self, TeaLogin},
};
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
    /// The agent has a `tea` wrapper, whose login stands for the agent on
    /// Forgejo and Gitea hosts.
    tea: bool,
    bitbucket: Credentials,
    /// The Bitbucket authorization, fetched once for this account's requests.
    bitbucket_authorization: OnceCell<Option<Vec<u8>>>,
}

/// How a Forgejo or Gitea request authenticates.
pub(super) enum ForgejoLogin {
    /// The token fj saved for the host.
    Token(String),
    /// tea's login for the host, which sends the request itself.
    Tea(TeaLogin),
    /// No login, which reaches only public repositories.
    Anonymous,
}

impl Account {
    /// The CLIs as Shoal finds them, the token fj saved in its default
    /// location, and the configured Bitbucket credentials.
    pub fn user(bitbucket: &Credentials) -> Self {
        Self {
            bitbucket: bitbucket.clone(),
            ..Self::default()
        }
    }

    /// The workspace's agent account; options it leaves unset use the user's
    /// logins, as the agent itself would.
    pub fn agent(
        config: &agent_auth::Config,
        paths: &Paths,
        git: &git_profile::Git,
        bitbucket: &Credentials,
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
            tea: config.tea.is_some(),
            bitbucket: bitbucket.clone(),
            bitbucket_authorization: OnceCell::new(),
        })
    }

    /// Select this account's `gh` for a request.
    pub(super) fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(launch) = &self.launch {
            launch.apply(command);
        }
    }

    /// The login for a Forgejo or Gitea `host`: fj's token, then tea's
    /// login. The user may have neither, which leaves an anonymous request;
    /// an agent with a wrapper must have a login through it.
    pub(super) async fn forgejo_login(&self, host: &str) -> Result<ForgejoLogin> {
        ensure!(
            !self.fj_home_missing,
            "set agent_auth.fj_home to the HOME the agent's fj wrapper uses, so Shoal can use the agent's Forgejo login"
        );
        if let Some(path) = &self.fj_keys {
            return fj_token(path, host)
                .map(ForgejoLogin::Token)
                .context("the agent's fj login");
        }
        if self.tea {
            return tea::login(self, host)
                .await
                .context("the agent's tea logins")?
                .map(ForgejoLogin::Tea)
                .with_context(|| format!("the agent's tea has no login for {host}"));
        }
        if let Ok(token) = fj_token(&fj_keys_path()?, host) {
            return Ok(ForgejoLogin::Token(token));
        }
        // tea is optional; without it or a login for the host, read anonymously.
        Ok(match tea::login(self, host).await {
            Ok(Some(login)) => ForgejoLogin::Tea(login),
            Ok(None) | Err(_) => ForgejoLogin::Anonymous,
        })
    }

    /// The curl config line that signs a Bitbucket request in, if any.
    pub(super) async fn bitbucket_authorization(&self) -> Result<Option<Vec<u8>>> {
        self.bitbucket_authorization
            .get_or_try_init(|| self.bitbucket.authorization())
            .await
            .cloned()
    }
}
