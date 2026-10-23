//! Whose credentials a forge request uses: the user's own logins, or an
//! agent's, which the caller chooses and describes.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use tokio::sync::OnceCell;

use crate::{
    backends::{
        bitbucket::Credentials,
        forgejo::tea::{self, TeaLogin},
    },
    transport::{
        api::{fj_keys_at, fj_keys_path, fj_token},
        memory::Memory,
    },
};
/// Starts a forge CLI as someone other than the user, such as through
/// their wrappers.
pub trait Launch: Send + Sync {
    fn apply(&self, command: &mut tokio::process::Command);
}

/// An agent's own logins; the user's stand in for those it leaves unset.
pub struct AgentLogins {
    /// Starts the agent's `gh`, `glab` and `tea`.
    pub launch: Box<dyn Launch>,
    /// The HOME the agent's `fj` runs with, holding its saved logins.
    pub fj_home: Option<PathBuf>,
    /// The agent has an `fj` wrapper.
    pub fj: bool,
    /// The agent has a `tea` wrapper.
    pub tea: bool,
}

#[derive(Default)]
pub struct Account {
    /// Starts the agent's CLIs; the user's when absent.
    launch: Option<Box<dyn Launch>>,
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
    /// What earlier requests as this account taught.
    memory: Memory,
}

/// How a Forgejo or Gitea request authenticates.
pub(crate) enum ForgejoLogin {
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

    /// An agent's account; logins it leaves unset use the user's, as the
    /// agent itself would.
    pub fn agent(logins: AgentLogins, bitbucket: &Credentials) -> Self {
        let fj_keys = logins.fj_home.map(|home| fj_keys_at(&home, None));
        Self {
            launch: Some(logins.launch),
            fj_home_missing: logins.fj && fj_keys.is_none(),
            fj_keys,
            tea: logins.tea,
            bitbucket: bitbucket.clone(),
            bitbucket_authorization: OnceCell::new(),
            memory: Memory::default(),
        }
    }

    /// This account, sharing what its requests teach with every other
    /// account given `memory`, which must act as the same account.
    pub fn remembering(self, memory: Memory) -> Self {
        Self { memory, ..self }
    }

    pub(crate) fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Select this account's CLI for a request.
    pub(crate) fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(launch) = &self.launch {
            launch.apply(command);
        }
    }

    /// The login for a Forgejo or Gitea `host`: fj's token, then tea's
    /// login. The user may have neither, which leaves an anonymous request;
    /// an agent with a wrapper must have a login through it.
    pub(crate) async fn forgejo_login(&self, host: &str) -> Result<ForgejoLogin> {
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
    pub(crate) async fn bitbucket_authorization(&self) -> Result<Option<Vec<u8>>> {
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
        let home = root.join("fj-home");
        let keys = fj_keys_at(&home, None);
        std::fs::create_dir_all(keys.parent().unwrap()).unwrap();
        std::fs::write(
            &keys,
            r#"{"hosts":{"fj.example":{"type":"Application","token":"fj-token"}}}"#,
        )
        .unwrap();
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tea = bin.join("tea");
        std::fs::write(
            &tea,
            "#!/bin/sh\necho '[{\"name\":\"agent\",\"url\":\"https://tea.example\"}]'\n",
        )
        .unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
        let logins = AgentLogins {
            launch: Box::new(Bin(bin)),
            fj_home: Some(home),
            fj: false,
            tea: true,
        };
        Account::agent(logins, &Default::default())
    }

    /// Finds the CLIs only in one directory.
    struct Bin(std::path::PathBuf);

    impl Launch for Bin {
        fn apply(&self, command: &mut tokio::process::Command) {
            command.env("PATH", &self.0);
        }
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
