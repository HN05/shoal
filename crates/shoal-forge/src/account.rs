//! Whose credentials a forge request uses: the user's own tokens, or an
//! agent's, which the caller chooses and describes.
use std::collections::HashMap;

use anyhow::{Result, ensure};
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
}

/// A login and its token, fetched once for the account's requests and
/// never kept beyond it.
struct Signer {
    login: Login,
    token: OnceCell<String>,
}

/// An agent's own logins; the user's stand in for those it leaves unset.
pub struct AgentLogins {
    /// The agent account's tokens.
    pub logins: Logins,
    /// The forges on which the agent has an account of its own, where the
    /// user's tokens never stand in for one it lacks.
    pub separate: Vec<ForgeKind>,
}

#[derive(Default)]
pub struct Account {
    logins: Logins,
    /// The user's tokens, for hosts an agent has none of its own for.
    fallback: Logins,
    separate: Vec<ForgeKind>,
    /// What earlier requests as this account taught.
    memory: Memory,
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
    /// the agent itself would, except on its separate forges.
    pub fn agent(agent: AgentLogins, user: Logins) -> Self {
        Self {
            logins: agent.logins,
            fallback: user,
            separate: agent.separate,
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

    /// The curl config line that signs a request to `repo`'s host in, or
    /// none without a token, which reaches only public repositories.
    pub(crate) async fn authorization(&self, repo: &ForgeRepo) -> Result<Option<Vec<u8>>> {
        let Some(signer) = self.signer(&repo.host, repo.kind)? else {
            return Ok(None);
        };
        signer.authorization(repo.kind).await.map(Some)
    }

    /// The login for `host`: the account's own, else the user's unless the
    /// agent has an account of its own on `kind`.
    fn signer(&self, host: &str, kind: ForgeKind) -> Result<Option<&Signer>> {
        if let Some(signer) = self.logins.get(host) {
            return Ok(Some(signer));
        }
        ensure!(
            !self.separate.contains(&kind),
            "the agent has its own {} account, so set its agent_token under [machine.forges.\"{host}\"]",
            kind.name()
        );
        Ok(self.fallback.get(host))
    }
}

impl Signer {
    /// The curl config line that signs a request to a `kind` forge in.
    async fn authorization(&self, kind: ForgeKind) -> Result<Vec<u8>> {
        let token = self
            .token
            .get_or_try_init(|| self.login.token.fetch())
            .await?;
        let token = token.trim();
        // The value goes into a curl config line; anything but a plain token is refused.
        ensure!(
            !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\')),
            "the token is empty or has an unexpected format"
        );
        let value = kind.authorization(token, self.login.email.as_deref());
        Ok(format!("header = \"Authorization: {value}\"\n").into_bytes())
    }
}

#[cfg(test)]
mod tests;
