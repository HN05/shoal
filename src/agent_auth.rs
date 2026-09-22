//! User-owned forge authentication wrappers, selected only for agent launches.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub fj: Option<PathBuf>,
    pub gh: Option<PathBuf>,
}

impl Config {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrappers_resolve_per_tool() {
        let global: crate::config::Config =
            toml::from_str("[agent_auth]\nfj = '~/bin/fj-agent'\ngh = '/global/gh-agent'\n")
                .unwrap();
        let layers = crate::repo_config::ConfigLayers {
            worktree_file: crate::repo_config::parse("[agent_auth]\nfj = '/repo/fj-agent'\n")
                .unwrap(),
            saved_repository_config: crate::repo_config::parse(
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
        assert!(crate::repo_config::parse("[agent_auth]\ngh = 'gh'").is_err());
        assert!(crate::repo_config::parse("[agent_auth]\nunknown = '/bin/gh'").is_err());
    }
}
