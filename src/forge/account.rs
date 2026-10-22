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
        if self.fj_keys.is_some() || self.tea {
            return self.agent_forgejo_login(host).await;
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

    /// The agent's fj token for `host`, else its tea login for the host. The
    /// user's logins never stand in for the agent's.
    async fn agent_forgejo_login(&self, host: &str) -> Result<ForgejoLogin> {
        let fj = self
            .fj_keys
            .as_ref()
            .map(|path| fj_token(path, host).context("the agent's fj login"));
        if let Some(Ok(token)) = fj {
            return Ok(ForgejoLogin::Token(token));
        }
        if self.tea
            && let Some(login) = tea::login(self, host)
                .await
                .context("the agent's tea logins")?
        {
            return Ok(ForgejoLogin::Tea(login));
        }
        match fj {
            Some(Err(error)) => Err(error),
            _ => anyhow::bail!("the agent's tea has no login for {host}"),
        }
    }

    /// The curl config line that signs a Bitbucket request in, if any.
    pub(super) async fn bitbucket_authorization(&self) -> Result<Option<Vec<u8>>> {
        self.bitbucket_authorization
            .get_or_try_init(|| self.bitbucket.authorization())
            .await
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// An agent with `fj_home`, whose fj has a login for `fj.example` only,
    /// and a `tea` wrapper with a login for `tea.example` only.
    fn agent(root: &std::path::Path) -> Account {
        let paths = Paths::for_test(root);
        paths.prepare().unwrap();
        let home = root.join("fj-home");
        let keys = fj_keys_at(&home, None);
        std::fs::create_dir_all(keys.parent().unwrap()).unwrap();
        std::fs::write(
            &keys,
            r#"{"hosts":{"fj.example":{"type":"Application","token":"fj-token"}}}"#,
        )
        .unwrap();
        let tea = root.join("tea-agent");
        std::fs::write(
            &tea,
            "#!/bin/sh\necho '[{\"name\":\"agent\",\"url\":\"https://tea.example\"}]'\n",
        )
        .unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = agent_auth::Config {
            fj_home: Some(home),
            tea: Some(tea),
            ..Default::default()
        };
        Account::agent(&config, &paths, &Default::default(), &Default::default()).unwrap()
    }

    #[tokio::test]
    async fn agents_fall_back_from_fj_to_tea_per_host() {
        let root = tempfile::tempdir().unwrap();
        let account = agent(root.path());
        assert!(matches!(
            account.forgejo_login("fj.example").await.unwrap(),
            ForgejoLogin::Token(token) if token == "fj-token"
        ));
        assert!(matches!(
            account.forgejo_login("tea.example").await.unwrap(),
            ForgejoLogin::Tea(login) if login.name == "agent"
        ));
        let error = account.forgejo_login("other.example").await.err().unwrap();
        assert!(
            format!("{error:#}").contains("the agent's fj login"),
            "{error:#}"
        );
    }
}
