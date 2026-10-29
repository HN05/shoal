//! The forge and the accounts item actions go out with: the user's own
//! logins for reads and the user's decisions, the agent account for the rest.
use anyhow::{Context, Result};

use crate::{
    config::{Config, resolve::Effective},
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::{Account, AgentLogins},
    },
    fsutil::expand_home,
    model::{Repository, Workspace},
};

/// The forge memory of the user's own logins.
const USER_LOGINS: &str = "user";

impl Manager {
    /// The user's own logins, which reads and the user's decisions use.
    pub(crate) fn user_account(&self) -> Account {
        Account::user(&self.config().bitbucket).remembering(self.forge_memories.of(USER_LOGINS))
    }

    pub(in crate::daemon::links) async fn agent_account(
        &self,
        workspace: &Workspace,
    ) -> Result<Account> {
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        self.agent_account_from(&config, &settings)
    }

    /// The agent account `settings` select, with the Git profiles of
    /// `config`, the snapshot they were resolved from.
    pub(in crate::daemon::links) fn agent_account_from(
        &self,
        config: &Config,
        settings: &Effective,
    ) -> Result<Account> {
        let auth = &settings.agent_auth;
        let logins = AgentLogins {
            launch: Box::new(auth.prepare(&self.paths, &config.git)?),
            fj_home: (auth.fj_home.as_ref()).map(|home| expand_home(home, &self.paths.home)),
            fj: auth.fj.is_some(),
            tea: auth.tea.is_some(),
        };
        // An agent with the user's logins shares what the user's requests taught.
        let identity = match auth.forge_identity()? {
            Some(logins) => format!("agent {logins}"),
            None => USER_LOGINS.to_owned(),
        };
        Ok(
            Account::agent(logins, &config.bitbucket)
                .remembering(self.forge_memories.of(&identity)),
        )
    }

    /// The forge of the workspace's `origin`.
    pub(in crate::daemon::links) async fn workspace_forge(
        &self,
        workspace: &Workspace,
    ) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(&workspace.path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }

    pub(in crate::daemon::links) async fn workspace_forge_account(
        &self,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            self.workspace_forge(workspace).await?,
            self.agent_account(workspace).await?,
        ))
    }

    /// The forge of the registered checkout's `origin`, with the agent
    /// account of the repository's settings.
    pub(in crate::daemon::links) async fn checkout_forge_account(
        &self,
        repository: &Repository,
    ) -> Result<(ForgeRepo, Account)> {
        let remote = crate::repository::remote_url_from_path(&repository.path)
            .await?
            .context("forge actions need an origin remote")?;
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self
            .repository_settings_from(&config, &repository.id)
            .await?;
        let account = self.agent_account_from(&config, &settings)?;
        Ok((self.forge(&remote).await?, account))
    }
}
