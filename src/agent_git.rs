//! The Git settings of agent launches only: the selected Git profile, the
//! agent token logins of [`crate::git_credential`], and the push rewrites of
//! [`crate::push_gate`].
use anyhow::{Context, Result};

#[derive(Default)]
pub struct Launch {
    git: Vec<(String, String)>,
}

impl Launch {
    /// The child's environment changes for the Git profile `git_profile`
    /// names; nested commands inherit it.
    pub fn prepare(git_profile: Option<&str>, git: &crate::git_profile::Git) -> Result<Self> {
        let git = match git_profile {
            Some(name) => git
                .profile(name)
                .and_then(|profile| profile.settings())
                .with_context(|| format!("agent.git_profile {name}"))?,
            None => Vec::new(),
        };
        Ok(Self { git })
    }

    /// The Git settings of an agent without a Git profile, such as a chat's:
    /// the credential helper for each host the global config gives an agent
    /// token.
    pub fn agent_logins(paths: &crate::paths::Paths) -> Result<Self> {
        let config = crate::config::Config::load(paths)?;
        let mut launch = Self::default();
        launch.add_agent_logins(paths, config.machine.forges.agent_token_hosts())?;
        Ok(launch)
    }

    /// Add the credential helper for each of `hosts`, the hosts with an
    /// agent token, that the profile's own credential helper leaves to it.
    pub fn add_agent_logins<'a>(
        &mut self,
        paths: &crate::paths::Paths,
        hosts: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let logins = crate::git_credential::settings(paths, hosts, &self.git)?;
        self.git.extend(logins);
        Ok(())
    }

    /// Add `settings` after the profile's.
    pub fn add(&mut self, settings: &[(String, String)]) {
        self.git.extend_from_slice(settings);
    }

    /// Profile settings follow inherited `GIT_CONFIG_*` entries and take
    /// precedence over every Git config file. Inherited author and committer
    /// variables would override the profile's identity, so they are dropped
    /// for each identity setting it names.
    pub fn apply(&self, command: &mut std::process::Command) {
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
