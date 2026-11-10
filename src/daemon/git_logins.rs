//! The agent account's logins for Git over HTTPS, which the Git credential
//! helper of an agent launch asks for.
use anyhow::Result;

use crate::{
    client::agents::GitCredential,
    config::forges::Whose,
    daemon::{scope::Caller, workspace::Manager},
};

impl Manager {
    /// The agent token `[machine.forges]` sets for `host`, with the username
    /// its forge takes from Git, for a scoped `caller`: only Shoal's agents
    /// and the pushes it runs act as the agent account. None for the user's
    /// own processes or a host without one. The token is read from the
    /// current configuration on each request.
    pub(crate) async fn agent_git_credential(
        &self,
        caller: Option<&Caller>,
        host: &str,
    ) -> Result<Option<GitCredential>> {
        let config = self.config();
        let forges = &config.machine.forges;
        if caller.is_none() || !forges.has_agent_token(host) {
            return Ok(None);
        }
        let username = self.host_type(host, "https").await?.kind().git_username();
        let logins = forges.logins(Whose::Agent);
        Ok(logins.token(host).await?.map(|password| GitCredential {
            username: username.to_owned(),
            password: password.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests;
