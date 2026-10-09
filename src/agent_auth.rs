//! User-owned forge authentication wrappers and a Git profile, selected only
//! for agent launches.
use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub fj: Option<PathBuf>,
    pub gh: Option<PathBuf>,
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
        if self.fj.is_none() && self.gh.is_none() {
            return Ok(None);
        }
        let directory = paths.agent_auth_directory()?;
        for (name, path) in [
            (Tool::Forgejo.program(), &self.fj),
            (Tool::GitHub.program(), &self.gh),
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
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned()).chain(std::env::split_paths(&inherited)),
        )?;
        Ok(Some(Wrappers {
            _directory: directory,
            path,
        }))
    }

    pub fn validate(&self) -> Result<()> {
        for (name, path) in [
            (Tool::Forgejo.program(), &self.fj),
            (Tool::GitHub.program(), &self.gh),
        ] {
            if let Some(path) = path {
                ensure!(
                    (path.is_absolute() || path.starts_with("~/"))
                        && !path.as_os_str().as_encoded_bytes().contains(&0),
                    "agent_auth.{name} must be an absolute or ~/ executable path"
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
    _directory: tempfile::TempDir,
    path: OsString,
}

impl Launch {
    /// Profile settings follow inherited `GIT_CONFIG_*` entries and take
    /// precedence over every Git config file. Inherited author and committer
    /// variables would override the profile's identity, so they are dropped
    /// for each identity setting it names.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(wrappers) = &self.wrappers {
            command.env("PATH", &wrappers.path);
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
mod tests {
    use super::*;

    #[test]
    fn wrappers_resolve_per_tool() {
        let global: crate::config::Config =
            toml::from_str("[agent_auth]\nfj = '~/bin/fj-agent'\ngh = '/global/gh-agent'\n")
                .unwrap();
        let layers = crate::config::repo::ConfigLayers {
            worktree_file: crate::config::repo::parse("[agent_auth]\nfj = '/repo/fj-agent'\n")
                .unwrap(),
            saved_repository_config: crate::config::repo::parse(
                "[agent_auth]\nfj = '/saved/fj-agent'\n",
            )
            .unwrap(),
        };
        let effective = global.resolve(&layers).unwrap();
        assert_eq!(effective.agent_auth.fj, Some("/saved/fj-agent".into()));
        assert_eq!(effective.agent_auth.gh, Some("/global/gh-agent".into()));
    }

    #[test]
    fn wrapper_paths_must_not_depend_on_the_launch_directory_or_path() {
        for path in ["", "fj", "./fj", "~other/fj"] {
            let config = Config {
                fj: Some(path.into()),
                ..Default::default()
            };
            assert!(config.validate().is_err(), "{path}");
        }
        assert!(crate::config::repo::parse("[agent_auth]\ngh = 'gh'").is_err());
        assert!(crate::config::repo::parse("[agent_auth]\nunknown = '/bin/gh'").is_err());
    }
}
