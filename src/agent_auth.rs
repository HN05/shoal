//! User-owned forge authentication wrappers and a Git profile, selected only
//! for agent launches.
use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub fj: Option<PathBuf>,
    /// The HOME the agent's `fj` runs with, holding the login Shoal uses for
    /// the agent's Forgejo API requests.
    pub fj_home: Option<PathBuf>,
    pub gh: Option<PathBuf>,
    /// The agent's `glab`, which sends GitLab requests as the agent.
    pub glab: Option<PathBuf>,
    /// The agent's `tea`, whose login stands for the agent on Gitea and on
    /// Forgejo hosts without `fj_home`.
    pub tea: Option<PathBuf>,
    pub git_profile: Option<String>,
}

impl Config {
    /// The child's environment changes for the agent's account; nested commands
    /// inherit it.
    pub fn prepare(
        &self,
        paths: &crate::paths::Paths,
        git: &crate::git_profile::Git,
    ) -> Result<Launch> {
        self.validate()?;
        let git = match &self.git_profile {
            Some(name) => git
                .profile(name)
                .and_then(|profile| profile.settings())
                .with_context(|| format!("agent_auth.git_profile {name}"))?,
            None => Vec::new(),
        };
        Ok(Launch {
            wrappers: self.wrappers(paths)?,
            git,
        })
    }

    /// Keep the private PATH directory alive for the tracked execution.
    fn wrappers(&self, paths: &crate::paths::Paths) -> Result<Option<Wrappers>> {
        if self.fj.is_none() && self.gh.is_none() && self.glab.is_none() && self.tea.is_none() {
            return Ok(None);
        }
        let directory = paths.agent_auth_directory()?;
        for (name, path) in [
            (Tool::Forgejo.program(), &self.fj),
            (Tool::GitHub.program(), &self.gh),
            (Tool::GitLab.program(), &self.glab),
            (Tool::Tea.program(), &self.tea),
        ] {
            let Some(path) = path else { continue };
            let path = crate::fsutil::expand_home(path, &paths.home);
            let executable = crate::fsutil::is_executable(&path)
                .with_context(|| format!("agent_auth.{name}: inspect {}", path.display()))?;
            ensure!(
                executable,
                "agent_auth.{name}: {} is not an executable file",
                path.display()
            );
            std::os::unix::fs::symlink(&path, directory.path().join(name))?;
        }
        std::env::join_paths([directory.path()])?;
        Ok(Some(Wrappers { directory }))
    }

    /// What tells the forge logins this configuration selects apart, or none
    /// when the agent uses the user's own.
    pub fn forge_identity(&self) -> Result<Option<String>> {
        let logins = Self {
            git_profile: None,
            ..self.clone()
        };
        if [
            &logins.fj,
            &logins.fj_home,
            &logins.gh,
            &logins.glab,
            &logins.tea,
        ]
        .iter()
        .all(|login| login.is_none())
        {
            return Ok(None);
        }
        Ok(Some(serde_json::to_string(&logins)?))
    }

    pub fn validate(&self) -> Result<()> {
        for (name, path) in [
            (Tool::Forgejo.program(), &self.fj),
            ("fj_home", &self.fj_home),
            (Tool::GitHub.program(), &self.gh),
            (Tool::GitLab.program(), &self.glab),
            (Tool::Tea.program(), &self.tea),
        ] {
            if let Some(path) = path {
                ensure!(
                    (path.is_absolute() || path.starts_with("~/"))
                        && !path.as_os_str().as_encoded_bytes().contains(&0),
                    "agent_auth.{name} must be an absolute or ~/ path"
                );
            }
        }
        if let Some(name) = &self.git_profile {
            crate::validate::name("git profile", name)?;
        }
        Ok(())
    }
}

pub struct Launch {
    wrappers: Option<Wrappers>,
    git: Vec<(String, String)>,
}

struct Wrappers {
    directory: tempfile::TempDir,
}

impl crate::forge::account::Launch for Launch {
    fn apply(&self, command: &mut tokio::process::Command) {
        Launch::apply(self, command);
    }
}

impl Launch {
    /// Profile settings follow inherited `GIT_CONFIG_*` entries and take
    /// precedence over every Git config file. Inherited author and committer
    /// variables would override the profile's identity, so they are dropped
    /// for each identity setting it names.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(wrappers) = &self.wrappers {
            crate::env::prepend_path(command, wrappers.directory.path().as_os_str());
        }
        if self.git.is_empty() {
            return;
        }
        let inherited = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        for (index, (key, value)) in self.git.iter().enumerate() {
            let index = inherited + index;
            command
                .env(format!("GIT_CONFIG_KEY_{index}"), key)
                .env(format!("GIT_CONFIG_VALUE_{index}"), value);
            for field in ["name", "email"] {
                if key.eq_ignore_ascii_case(&format!("user.{field}")) {
                    let field = field.to_ascii_uppercase();
                    command
                        .env_remove(format!("GIT_AUTHOR_{field}"))
                        .env_remove(format!("GIT_COMMITTER_{field}"));
                }
            }
        }
        command.env("GIT_CONFIG_COUNT", (inherited + self.git.len()).to_string());
    }
}

#[cfg(test)]
mod tests;
