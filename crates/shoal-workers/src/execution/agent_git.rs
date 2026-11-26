//! The Git settings of agent launches only: the selected Git profile, the
//! agent token logins of [`git_credential`], and the push rewrites of
//! [`push_gate`].
pub(super) mod git_credential;
pub(super) mod push_gate;

use anyhow::{Context, Result};

#[derive(Default)]
pub struct Launch {
    git: Vec<(String, String)>,
}

impl Launch {
    /// The child's environment changes for the Git profile `git_profile`
    /// names; nested commands inherit it.
    pub fn prepare(
        git_profile: Option<&str>,
        git: &crate::config::git_profiles::Git,
    ) -> Result<Self> {
        let git = match git_profile {
            Some(name) => git
                .profile(name)
                .and_then(|profile| profile.settings())
                .with_context(|| format!("agent.git_profile {name}"))?,
            None => Vec::new(),
        };
        Ok(Self { git })
    }

    /// Add the credential helper for each of `hosts`, the hosts with an
    /// agent token, that the profile's own credential helper leaves to it.
    pub fn add_agent_logins<'a>(
        &mut self,
        paths: &crate::paths::Paths,
        hosts: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let logins = git_credential::settings(paths, hosts, &self.git)?;
        self.git.extend(logins);
        Ok(())
    }

    pub fn add(&mut self, settings: &[(String, String)]) {
        self.git.extend_from_slice(settings);
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.git.retain(|(key, _)| keep(key));
    }

    /// Profile settings follow inherited `GIT_CONFIG_*` entries and take
    /// precedence over every Git config file.
    pub fn apply(&self, command: &mut std::process::Command) {
        crate::git::config_env::append(command, &self.git);
    }
}
