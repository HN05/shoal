//! Whose credentials a forge request uses: the user's own tokens, or an
//! agent's, which the caller chooses and describes.
use std::collections::HashMap;

use anyhow::{Result, bail, ensure};
use futures_util::future::BoxFuture;
use tokio::sync::OnceCell;

use crate::{
    ForgeRepo,
    backends::{Backend, ForgeKind},
    transport::memory::Memory,
};
/// Fetches the token of one login, such as by running a command; the
/// caller owns where it comes from and what a failure names.
pub trait Token: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<String>>;
}

/// How an account signs in to one host.
pub struct Login {
    pub token: Box<dyn Token>,
    /// The account an API token belongs to, for a forge that signs in
    /// with both.
    pub email: Option<String>,
}

/// What a request without a login for a host says: which token is
/// missing and where to set it.
pub type Missing = Box<dyn Fn(&str) -> String + Send + Sync>;

/// An account's logins by host.
#[derive(Default)]
pub struct Logins(HashMap<String, Signer>);

impl Logins {
    /// Sign in to `host`, as remotes name it, with `login`.
    pub fn insert(&mut self, host: &str, login: Login) {
        let signer = Signer {
            login,
            token: OnceCell::new(),
        };
        self.0.insert(host.to_ascii_lowercase(), signer);
    }

    fn get(&self, host: &str) -> Option<&Signer> {
        self.0.get(&host.to_ascii_lowercase())
    }

    /// The token of the login for `host`, or none without one.
    pub async fn token(&self, host: &str) -> Result<Option<&str>> {
        match self.get(host) {
            Some(signer) => signer.token().await.map(Some),
            None => Ok(None),
        }
    }
}

/// A login and its token, fetched once for the account's requests and
/// never kept beyond it.
struct Signer {
    login: Login,
    token: OnceCell<String>,
}

#[derive(Default)]
pub struct Account {
    logins: Logins,
    /// The user's tokens, for hosts an agent has none of its own for.
    fallback: Logins,
    /// What earlier requests as this account taught.
    memory: Memory,
    /// What a change says for a host the account has no login for.
    missing: Option<Missing>,
}

impl Account {
    /// The user's tokens.
    pub fn user(logins: Logins) -> Self {
        Self {
            logins,
            ..Self::default()
        }
    }

    /// An agent's account; hosts it has no token for use the user's, as
    /// the agent itself would.
    pub fn agent(agent: Logins, user: Logins) -> Self {
        Self {
            logins: agent,
            fallback: user,
            ..Self::default()
        }
    }

    /// This account, sharing what its requests teach with every other
    /// account given `memory`, which must act as the same account.
    pub fn remembering(self, memory: Memory) -> Self {
        Self { memory, ..self }
    }

    /// This account, refusing a change to a host it has no login for
    /// with what `missing` says for that host.
    pub fn naming_missing(self, missing: Missing) -> Self {
        Self {
            missing: Some(missing),
            ..self
        }
    }

    pub(crate) fn memory(&self) -> &Memory {
        &self.memory
    }

    /// The curl config line that signs a request to `repo`'s host in, or
    /// none without a token, which reaches only public repositories.
    pub(crate) async fn authorization(&self, repo: &ForgeRepo) -> Result<Option<Vec<u8>>> {
        let Some(signer) = self.signer(&repo.host) else {
            return Ok(None);
        };
        signer.authorization(repo.kind).await.map(Some)
    }

    /// Refuse a change to `repo` without a login for its host, which the
    /// forge would refuse anyway, saying which token is missing.
    pub(crate) fn require_login(&self, repo: &ForgeRepo) -> Result<()> {
        if self.signer(&repo.host).is_some() {
            return Ok(());
        }
        match &self.missing {
            Some(missing) => bail!("{}", missing(&repo.host)),
            None => bail!("no token for {}", repo.host),
        }
    }

    /// The login for `host`: the account's own, else the user's.
    fn signer(&self, host: &str) -> Option<&Signer> {
        self.logins.get(host).or_else(|| self.fallback.get(host))
    }
}

impl Signer {
    /// The curl config line that signs a request to a `kind` forge in.
    async fn authorization(&self, kind: ForgeKind) -> Result<Vec<u8>> {
        let token = self.token().await?;
        let value = kind.authorization(token, self.login.email.as_deref());
        Ok(format!("header = \"Authorization: {value}\"\n").into_bytes())
    }

    /// The login's token, fetched once. It goes into a curl config line and
    /// a Git credential, so anything but a plain token is refused.
    async fn token(&self) -> Result<&str> {
        let token = self
            .token
            .get_or_try_init(|| self.login.token.fetch())
            .await?;
        let token = token.trim();
        ensure!(
            !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\')),
            "the token is empty or has an unexpected format"
        );
        Ok(token)
    }
}

#[cfg(test)]
mod tests;
