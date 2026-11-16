//! Secrets that user-configured commands fetch for tracked agents. Shoal keeps
//! the commands, never the values.
use crate::paths::PathsExt;
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
};

use crate::{env, paths::Paths};
use shoal_protocol::approvals::SecretSpecification;
pub use shoal_protocol::secrets::Secret;
pub use shoal_protocol::secrets::Secrets;

/// What a grant for `secret` binds: the command that fetches it and the tools
/// it serves.
pub fn specification(secret: &Secret) -> SecretSpecification {
    SecretSpecification {
        command: secret.command.clone(),
        tools: secret.tools.clone(),
    }
}

/// The tool-scoped secrets a launcher directory serves, beside its launchers.
/// Tool names cannot start with a dot, so no launcher can take its name.
pub const MANIFEST: &str = ".secrets.json";

/// The secret variables an agent launch adds to its child's environment, and
/// the launchers that give tool-scoped secrets to their tools.
#[derive(Debug, Default)]
pub struct Launch {
    environment: Vec<(String, OsString)>,
    launchers: Option<tempfile::TempDir>,
}

impl Launch {
    /// Fetch every environment secret now, so a failing command stops the
    /// launch before the agent starts. Tool-scoped secrets are fetched each
    /// time their tool runs.
    pub async fn prepare(secrets: &Secrets, paths: &Paths) -> Result<Self> {
        validate(secrets)?;
        let (scoped, unscoped): (Secrets, Secrets) = secrets
            .clone()
            .into_iter()
            .partition(|(_, secret)| !secret.tools.is_empty());
        let mut environment = Vec::new();
        for (name, secret) in &unscoped {
            environment.push((name.clone(), fetch(name, secret, None).await?));
        }
        let launchers = if scoped.is_empty() {
            None
        } else {
            Some(write_launchers(&scoped, paths)?)
        };
        Ok(Self {
            environment,
            launchers,
        })
    }

    /// The environment secrets, each with its value.
    pub fn environment(&self) -> impl Iterator<Item = (String, OsString)> + '_ {
        self.environment.iter().cloned()
    }

    /// Put the launchers first on the child's PATH, ahead of directories an
    /// earlier step prepended.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if !self.environment.is_empty() {
            for (name, value) in &self.environment {
                command.env(name, value);
            }
            command.env(
                env::SECRET_ENV,
                exported_names(self.environment.iter().map(|(name, _)| name)),
            );
        }
        if let Some(launchers) = &self.launchers {
            // `prepare` rejects a launcher directory that cannot join a PATH.
            let launcher = launchers.path().as_os_str();
            let inherited = std::env::var_os(env::SECRET_TOOLS).unwrap_or_default();
            if let Ok(directories) = env::joined_path(launcher, &inherited) {
                env::prepend_path(command, launcher);
                command.env(env::SECRET_TOOLS, directories);
            }
        }
    }
}

/// One executable per tool that hands its arguments to the secret tool
/// worker, beside the manifest it reads.
fn write_launchers(secrets: &Secrets, paths: &Paths) -> Result<tempfile::TempDir> {
    let directory = paths.secret_tools_directory()?;
    std::env::join_paths([directory.path()]).with_context(|| {
        format!(
            "secret tool launchers cannot go on PATH from {}",
            directory.path().display()
        )
    })?;
    std::fs::write(
        directory.path().join(MANIFEST),
        serde_json::to_vec(secrets)?,
    )?;
    let tools: BTreeSet<_> = secrets.values().flat_map(|secret| &secret.tools).collect();
    for tool in tools {
        let words = crate::workers::internal_command(
            paths,
            false,
            crate::workers::Worker::SecretTool {
                launchers: directory.path(),
                tool,
            },
        )?
        .into_iter()
        .map(|word| {
            word.into_string()
                .map_err(|word| anyhow::anyhow!("{} is not UTF-8", word.to_string_lossy()))
        })
        .collect::<Result<Vec<_>>>()?;
        let invocation = crate::shell::quote(&words);
        let path = directory.path().join(tool);
        std::fs::write(&path, format!("#!/bin/sh\nexec {invocation} \"$@\"\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(directory)
}

/// The secrets `settings` give agents: every one but those a webhook
/// trigger of `settings` or of the global `config` signs requests with,
/// which would let an agent sign its own and start spaces as the user.
pub fn for_agents(config: &crate::config::Config, settings: &crate::config::Settings) -> Secrets {
    let triggers = (settings.triggers.values()).chain(config.triggers().values());
    let signing: BTreeSet<&str> = triggers
        .filter_map(|trigger| trigger.signing_secret())
        .collect();
    (settings.secrets.iter())
        .filter(|(name, _)| !signing.contains(name.as_str()))
        .map(|(name, secret)| (name.clone(), secret.clone()))
        .collect()
}

/// [`env::SECRET_ENV`] after adding `names` to the inherited ones.
pub fn exported_names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut all = env::inherited_secret_exports();
    all.extend(names.cloned());
    all.sort();
    all.dedup();
    all.join(":")
}

/// Fetch the secret's value with PATH replaced by `path`.
pub async fn fetch(name: &str, secret: &Secret, path: Option<&OsStr>) -> Result<OsString> {
    crate::config::value::command_output(&secret.command, path)
        .await
        .with_context(|| format!("secrets.{name}"))
}

/// Configured secrets name variables Shoal may set, with commands it can run.
pub(crate) fn validate(secrets: &Secrets) -> Result<()> {
    for (name, secret) in secrets {
        crate::env::validate_configured_name("[secrets]", name)?;
        ensure!(
            !crate::env::PROTECTED.contains(&name.as_str()),
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

#[cfg(test)]
mod tests;
