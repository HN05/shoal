//! User-owned forge authentication wrappers, selected only for agent launches.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub fj: Option<PathBuf>,
    pub gh: Option<PathBuf>,
}

impl Config {
    /// Keep the private PATH directory alive for the tracked execution, including
    /// nested commands. Only the child's environment changes.
    pub fn prepare(&self, paths: &crate::paths::Paths) -> Result<Option<Launch>> {
        if self.fj.is_none() && self.gh.is_none() {
            return Ok(None);
        }
        self.validate()?;
        let directory = paths.agent_auth_directory()?;
        for (name, path) in [("fj", &self.fj), ("gh", &self.gh)] {
            let Some(path) = path else { continue };
            let path = match path.strip_prefix("~/") {
                Ok(relative) => paths.home.join(relative),
                Err(_) => path.clone(),
            };
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("agent_auth.{name}: inspect {}", path.display()))?;
            ensure!(
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
                "agent_auth.{name}: {} is not an executable file",
                path.display()
            );
            std::os::unix::fs::symlink(&path, directory.path().join(name))?;
        }
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned()).chain(std::env::split_paths(&inherited)),
        )?;
        Ok(Some(Launch {
            _directory: directory,
            path,
        }))
    }

    pub fn validate(&self) -> Result<()> {
        for (name, path) in [("fj", &self.fj), ("gh", &self.gh)] {
            if let Some(path) = path {
                ensure!(
                    (path.is_absolute() || path.starts_with("~/"))
                        && !path.as_os_str().as_encoded_bytes().contains(&0),
                    "agent_auth.{name} must be an absolute or ~/ executable path"
                );
            }
        }
        Ok(())
    }

    pub fn over(self, base: Self) -> Self {
        Self {
            fj: self.fj.or(base.fj),
            gh: self.gh.or(base.gh),
        }
    }
}

pub struct Launch {
    _directory: tempfile::TempDir,
    pub path: OsString,
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
        let effective = global.effective(&layers.clone().resolve()).unwrap();
        assert_eq!(effective.agent_auth.fj, Some("/saved/fj-agent".into()));
        assert_eq!(effective.agent_auth.gh, Some("/global/gh-agent".into()));
    }

    #[test]
    fn wrapper_paths_must_not_depend_on_the_launch_directory_or_path() {
        for path in ["", "fj", "./fj", "~other/fj"] {
            let config = Config {
                fj: Some(path.into()),
                gh: None,
            };
            assert!(config.validate().is_err(), "{path}");
        }
        assert!(crate::config::repo::parse("[agent_auth]\ngh = 'gh'").is_err());
        assert!(crate::config::repo::parse("[agent_auth]\nunknown = '/bin/gh'").is_err());
    }
}
