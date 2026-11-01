//! The Git settings of agent launches only: the selected Git profile, and
//! the push rewrites of [`crate::push_gate`].
use anyhow::{Context, Result};

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

    /// Add `settings` after the profile's.
    pub fn add(&mut self, settings: &[(String, String)]) {
        self.git.extend_from_slice(settings);
    }

    /// Profile settings follow inherited `GIT_CONFIG_*` entries and take
    /// precedence over every Git config file. Inherited author and committer
    /// variables would override the profile's identity, so they are dropped
    /// for each identity setting it names.
    pub fn apply(&self, command: &mut tokio::process::Command) {
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
