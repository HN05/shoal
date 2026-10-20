//! Whose credentials a forge request uses: the user's own logins, or a
//! workspace's `[agent_auth]` account.
use std::{collections::BTreeMap, sync::Mutex};

use anyhow::{Result, ensure};

use tokio::sync::OnceCell;

use super::{
    bitbucket::Credentials,
    token::{self, ForgejoHosts},
};
use crate::{agent_auth, git_profile, paths::Paths};

#[derive(Default)]
pub(crate) struct Account {
    /// The agent's `gh` wrapper and Git profile; the user's when absent.
    launch: Option<agent_auth::Launch>,
    /// The token command of each Forgejo or Gitea host, under the config
    /// key that sets it.
    forgejo: BTreeMap<String, (String, Vec<String>)>,
    /// The agent has an `fj` or `tea` wrapper, so a host without an agent
    /// token must not be read with the user's.
    forgejo_wrapper: bool,
    /// Tokens fetched for this account's requests, by host.
    forgejo_tokens: Mutex<BTreeMap<String, String>>,
    bitbucket: Credentials,
    /// The Bitbucket authorization, fetched once for this account's requests.
    bitbucket_authorization: OnceCell<Option<Vec<u8>>>,
}

impl Account {
    /// The CLIs as Shoal finds them, and the configured Forgejo and
    /// Bitbucket credentials.
    pub fn user(forgejo: &ForgejoHosts, bitbucket: &Credentials) -> Self {
        Self {
            forgejo: token_commands("forgejo", forgejo),
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
        forgejo: &ForgejoHosts,
        bitbucket: &Credentials,
    ) -> Result<Self> {
        let launch = config.prepare(paths, git)?;
        let forgejo_wrapper = config.fj.is_some() || config.tea.is_some();
        // The agent's own fj or tea must not be stood in for by the user's token.
        let mut tokens = if forgejo_wrapper {
            BTreeMap::new()
        } else {
            token_commands("forgejo", forgejo)
        };
        tokens.extend(token_commands("agent_auth.forgejo", &config.forgejo));
        Ok(Self {
            launch: Some(launch),
            forgejo: tokens,
            forgejo_wrapper,
            forgejo_tokens: Mutex::default(),
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

    /// The token for a Forgejo or Gitea `host`, or none, which reaches only
    /// public repositories. An agent with an `fj` or `tea` wrapper needs its
    /// own token for the host.
    pub(super) async fn forgejo_token(&self, host: &str) -> Result<Option<String>> {
        if let Some(token) = self.cached_forgejo_token(host) {
            return Ok(Some(token));
        }
        let Some((key, command)) = self.forgejo.get(host) else {
            ensure!(
                !self.forgejo_wrapper,
                "set [agent_auth.forgejo.\"{host}\"] token_command to the agent's token for {host}, so Shoal acts as the agent there"
            );
            return Ok(None);
        };
        let token = token::fetch(key, command).await?;
        self.forgejo_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(host.to_owned(), token.clone());
        Ok(Some(token))
    }

    fn cached_forgejo_token(&self, host: &str) -> Option<String> {
        self.forgejo_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(host)
            .cloned()
    }

    /// The curl config line that signs a Bitbucket request in, if any.
    pub(super) async fn bitbucket_authorization(&self) -> Result<Option<Vec<u8>>> {
        self.bitbucket_authorization
            .get_or_try_init(|| self.bitbucket.authorization())
            .await
            .cloned()
    }
}

/// Each host's token command under the config key that sets it.
fn token_commands(table: &str, hosts: &ForgejoHosts) -> BTreeMap<String, (String, Vec<String>)> {
    hosts
        .iter()
        .map(|(host, login)| {
            let key = format!("{table}.\"{host}\".token_command");
            (host.clone(), (key, login.token_command.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn hosts(toml: &str) -> ForgejoHosts {
        toml::from_str(toml).unwrap()
    }

    /// An agent with an `fj` wrapper and its own token for `agent.example`,
    /// whose user has tokens for `agent.example` and `user.example`.
    fn agent(root: &std::path::Path, fj: Option<&str>) -> Account {
        let paths = Paths::for_test(root);
        paths.prepare().unwrap();
        let config = agent_auth::Config {
            fj: fj.map(Into::into),
            forgejo: hosts("[\"agent.example\"]\ntoken_command = ['echo', 'agent-token']\n"),
            ..Default::default()
        };
        let user = hosts(
            "[\"agent.example\"]\ntoken_command = ['echo', 'user-token']\n\
             [\"user.example\"]\ntoken_command = ['echo', 'user-token']\n",
        );
        Account::agent(
            &config,
            &paths,
            &Default::default(),
            &user,
            &Default::default(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn agents_use_their_own_tokens_and_wrappers_refuse_the_users() {
        let root = tempfile::tempdir().unwrap();
        let account = agent(root.path(), None);
        assert_eq!(
            account
                .forgejo_token("agent.example")
                .await
                .unwrap()
                .unwrap(),
            "agent-token"
        );
        // Without a wrapper, a host the agent has no token for uses the user's.
        assert_eq!(
            account
                .forgejo_token("user.example")
                .await
                .unwrap()
                .unwrap(),
            "user-token"
        );
        assert_eq!(account.forgejo_token("other.example").await.unwrap(), None);

        let fj = root.path().join("fj-agent");
        std::fs::write(&fj, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fj, std::fs::Permissions::from_mode(0o755)).unwrap();
        let account = agent(root.path(), fj.to_str());
        assert_eq!(
            account
                .forgejo_token("agent.example")
                .await
                .unwrap()
                .unwrap(),
            "agent-token"
        );
        let error = account.forgejo_token("user.example").await.unwrap_err();
        assert!(
            format!("{error:#}").contains("agent_auth.forgejo"),
            "{error:#}"
        );
    }
}
