//! `[machine.forges."<host>"]`: what a forge host serves, set instead of
//! asking it, and the tokens the user and the agent account sign in with.
use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::value::Value;
use crate::forge::{
    account::{Login, Logins, Token},
    backends::ForgeKind,
};

/// Bounds a token command, which may wait on a password manager.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct Forges(BTreeMap<String, Forge>);

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Forge {
    /// The forge's API, pinned so the host is never asked.
    #[serde(rename = "type")]
    pub kind: Option<ForgeKind>,
    /// The user's token for the host.
    pub token: Option<Value>,
    /// The agent account's token for the host.
    pub agent_token: Option<Value>,
    /// The account a Bitbucket API token belongs to.
    pub email: Option<String>,
    pub agent_email: Option<String>,
}

/// Whose tokens a set of logins holds.
#[derive(Clone, Copy)]
pub enum Whose {
    User,
    Agent,
}

impl Forges {
    /// The type set for `host`, a lowercase host as remotes name it.
    pub fn pinned(&self, host: &str) -> Option<ForgeKind> {
        self.0
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(host))
            .and_then(|(_, forge)| forge.kind)
    }

    /// The type set for `host`, else its well-known one.
    pub fn settled(&self, host: &str) -> Option<ForgeKind> {
        self.pinned(host).or_else(|| ForgeKind::named(host))
    }

    /// Each pinned host and its type.
    pub fn pins(&self) -> Vec<(String, ForgeKind)> {
        self.0
            .iter()
            .filter_map(|(host, forge)| Some((host.clone(), forge.kind?)))
            .collect()
    }

    /// The logins of `whose` tokens, each fetched when a request first
    /// needs it.
    pub fn logins(&self, whose: Whose) -> Logins {
        let mut logins = Logins::default();
        for (host, forge) in &self.0 {
            let (value, email) = forge.credentials(whose);
            if let Some(value) = value {
                let token = Box::new(Configured {
                    setting: format!("machine.forges.{host:?}.{}", whose.token_key()),
                    value: value.clone(),
                });
                let email = email.clone();
                logins.insert(host, Login { token, email });
            }
        }
        logins
    }

    /// The hosts the agent account has a token of its own for.
    pub fn agent_token_hosts(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, forge)| forge.agent_token.is_some())
            .map(|(host, _)| host.as_str())
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for (host, forge) in &self.0 {
            ensure!(
                seen.insert(host.to_ascii_lowercase()),
                "machine.forges.{host:?} names a host another [machine.forges] entry already names in other letter case"
            );
            ensure!(
                !host.is_empty()
                    && host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')),
                "machine.forges.{host:?} must name a host, such as \"git.example.com\" or \"git.example.com:3000\""
            );
            forge.validate(host, self.settled(host))?;
        }
        Ok(())
    }
}

impl Forge {
    fn credentials(&self, whose: Whose) -> (Option<&Value>, &Option<String>) {
        match whose {
            Whose::User => (self.token.as_ref(), &self.email),
            Whose::Agent => (self.agent_token.as_ref(), &self.agent_email),
        }
    }

    fn validate(&self, host: &str, kind: Option<ForgeKind>) -> Result<()> {
        for whose in [Whose::User, Whose::Agent] {
            let (token, email) = self.credentials(whose);
            let setting = |key: &str| format!("machine.forges.{host:?}.{key}");
            if let Some(token) = token {
                token.validate(&setting(whose.token_key()))?;
            }
            let Some(email) = email else { continue };
            let key = whose.email_key();
            ensure!(
                kind == Some(ForgeKind::Bitbucket),
                "{} is for Bitbucket API tokens; set it only on bitbucket.org or a host whose type is \"bitbucket\"",
                setting(key)
            );
            ensure!(
                token.is_some(),
                "{} needs {}",
                setting(key),
                setting(whose.token_key())
            );
            ensure!(
                email.contains('@') && email.chars().all(|c| c.is_ascii_graphic()),
                "{} must be an email address",
                setting(key)
            );
        }
        Ok(())
    }
}

impl Whose {
    fn token_key(self) -> &'static str {
        match self {
            Self::User => "token",
            Self::Agent => "agent_token",
        }
    }

    fn email_key(self) -> &'static str {
        match self {
            Self::User => "email",
            Self::Agent => "agent_email",
        }
    }
}

/// A token as the config gives it: written, from this process's
/// environment, or printed by a command.
struct Configured {
    setting: String,
    value: Value,
}

impl Token for Configured {
    fn fetch(&self) -> futures_util::future::BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let token = self.value.resolve(|name| std::env::var_os(name));
            let token = match tokio::time::timeout(TOKEN_TIMEOUT, token).await {
                Ok(token) => token,
                Err(_) => Err(anyhow::anyhow!("timed out after a minute")),
            };
            token.with_context(|| format!("read {}", self.setting))
        })
    }
}

/// The setting that pins `host`'s type, for messages that ask for it.
pub fn setting(host: &str) -> String {
    format!(
        "set its type to \"github\", \"gitlab\", \"forgejo\" or \"bitbucket\" under [machine.forges.\"{host}\"] in the global config"
    )
}

#[cfg(test)]
mod tests;
