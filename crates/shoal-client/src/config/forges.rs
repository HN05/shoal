//! `[machine.forges."<host>"]`: what a forge host serves, set instead of
//! asking it, and the tokens the user and the agent account sign in with.
use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::value::Value;
use crate::forge::{
    account::{Login, Logins, Missing, Token},
    backends::ForgeKind,
};

/// Bounds a token command, which may wait on a password manager.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(60);

/// The most a token command may print.
const MAX_TOKEN_BYTES: usize = 64 * 1024;

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
    /// The account a token belongs to, where the forge signs it in with
    /// the account's email.
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

    /// Whether the agent account has a token of its own for `host`, rather
    /// than signing in with the user's.
    pub fn has_agent_token(&self, host: &str) -> bool {
        self.0
            .iter()
            .any(|(name, forge)| name.eq_ignore_ascii_case(host) && forge.agent_token.is_some())
    }

    /// The hosts the agent account has a token of its own for.
    pub fn agent_token_hosts(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter(|(_, forge)| forge.agent_token.is_some())
            .map(|(host, _)| host.as_str())
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

    /// What tells the token settings of `whose` apart, empty without any,
    /// so a changed setting names another account; a written token stands
    /// as its hash.
    pub fn identity(&self, whose: Whose) -> String {
        self.0
            .iter()
            .filter_map(|(host, forge)| {
                let (token, email) = forge.credentials(whose);
                let source = match token? {
                    Value::Written(token) => {
                        let mut hasher = std::hash::DefaultHasher::new();
                        std::hash::Hash::hash(token, &mut hasher);
                        format!("written {:x}", std::hash::Hasher::finish(&hasher))
                    }
                    other => format!("{other:?}"),
                };
                Some(format!("{host} {source} {email:?}"))
            })
            .collect::<Vec<_>>()
            .join("; ")
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
                kind.is_some_and(ForgeKind::uses_account_email),
                "{} is for tokens that sign in with their account's email; set it only on a host whose type is {}",
                setting(key),
                type_names(ForgeKind::uses_account_email)
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
    /// What a change to a host without a token of `self`'s says: which token
    /// to set, and that the daemon reads the global config only on reload.
    pub fn missing(self) -> Missing {
        let tokens = match self {
            Self::User => "token",
            Self::Agent => "agent_token, or your own token,",
        };
        Box::new(move |host| {
            format!(
                "no token for {host}: set {tokens} under [machine.forges.\"{host}\"] in the global config, then run `shoal daemon reload`"
            )
        })
    }

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
            let token = match &self.value {
                Value::Cmd { cmd } => token_output(cmd).await,
                value => value.resolve(|name| std::env::var_os(name)).await,
            };
            token.with_context(|| format!("read {}", self.setting))
        })
    }
}

/// What a token command prints, bounded in time and size; its diagnostics
/// go into the error rather than to the process's stderr.
async fn token_output(command: &[String]) -> Result<String> {
    let (program, args) = command
        .split_first()
        .context("the command names no program")?;
    let mut process = tokio::process::Command::new(program);
    process.args(args);
    shoal_subprocess::Run::new(process)
        .timeout(TOKEN_TIMEOUT)
        .output_limit(MAX_TOKEN_BYTES)
        .output()
        .await
}

/// The setting that pins `host`'s type, for messages that ask for it.
pub fn setting(host: &str) -> String {
    format!(
        "set its type to {} under [machine.forges.\"{host}\"] in the global config",
        type_names(|_| true)
    )
}

/// The quoted names of the forge types `selected` keeps, as a list ending
/// in "or".
fn type_names(selected: fn(ForgeKind) -> bool) -> String {
    let names: Vec<_> = ForgeKind::ALL
        .iter()
        .filter(|kind| selected(**kind))
        .map(|kind| format!("{:?}", kind.as_str()))
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
