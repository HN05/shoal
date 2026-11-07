//! Secrets: what a repository configures, and access to the ones that need
//! approval.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    approvals::{Lifetime, SecretSpecification},
    env,
};

/// A request to fetch a configured secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretRequest {
    pub reason: Option<String>,
    /// The source the caller will fetch from, when it was resolved earlier: a
    /// tool launcher serves its agent launch's configuration.
    pub source: Option<SecretSpecification>,
}

/// The caller may fetch the named secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretGrant {
    pub name: String,
    /// False when the secret needs no decision for this caller.
    pub approved: bool,
}

/// A configured secret without the command that fetches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSecret {
    pub name: String,
    /// Programs that receive the value; empty means the agent's environment.
    pub tools: Vec<String>,
    pub requires_approval: bool,
    pub approval_lifetime: Lifetime,
}

/// The secrets configured for a workspace's agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretCatalog {
    pub secrets: Vec<CatalogSecret>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Secret {
    /// Argument array whose standard output is the value.
    pub command: Vec<String>,
    /// Programs that receive the value each time they run; empty gives it to
    /// the agent's environment instead.
    pub tools: Vec<String>,
    /// Scoped callers fetch it only after the user approves a request.
    pub requires_approval: bool,
    pub approval_lifetime: Lifetime,
}

/// Configured secrets keyed by the variable that carries each value.
pub type Secrets = BTreeMap<String, Secret>;

pub fn validate(secrets: &Secrets) -> Result<()> {
    for (name, secret) in secrets {
        env::validate_configured_name("[secrets]", name)?;
        ensure!(
            !env::PROTECTED.contains(&name.as_str()),
            "[secrets] cannot set {name}"
        );
        crate::config::value::validate_command(
            &format!("secrets.{name}.command"),
            &secret.command,
        )?;
        // Environment secrets are fetched before the agent can ask for them.
        ensure!(
            !secret.requires_approval || !secret.tools.is_empty(),
            "secrets.{name}.requires_approval needs tools: an agent's environment is fetched before it can ask"
        );
        for tool in &secret.tools {
            ensure!(
                !tool.is_empty() && !tool.starts_with('.') && !tool.contains(['/', '\0']),
                "secrets.{name}.tools: {tool:?} is not a program name"
            );
        }
    }
    Ok(())
}
