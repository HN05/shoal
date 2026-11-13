//! Named Git identities, `[machine.git_profiles]`, which a repository config
//! or `add --git-profile` selects.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::validate;

/// `[machine.git_profiles]`: the profiles a repository config or
/// `add --git-profile` may select by name.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Git {
    pub profiles: BTreeMap<String, Profile>,
}

impl Git {
    pub fn validate(&self) -> Result<()> {
        for (name, profile) in &self.profiles {
            validate::name("git profile", name)?;
            profile
                .settings()
                .with_context(|| format!("git profile {name}"))?;
        }
        Ok(())
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles.get(name).with_context(|| {
            if self.profiles.is_empty() {
                format!(
                    "git profile {name} is not defined; add [machine.git_profiles.{name}] to the global config"
                )
            } else {
                let defined: Vec<_> = self.profiles.keys().map(String::as_str).collect();
                format!(
                    "git profile {name} is not defined; the global config defines {}",
                    defined.join(", ")
                )
            }
        })
    }
}

/// Git settings as TOML: `user.email = "…"` or an `email` key in a `[user]`
/// table both name `user.email`. Values are strings, integers or booleans.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct Profile(toml::Table);

impl Profile {
    /// The `(key, value)` pairs for `git config`, in key order.
    pub fn settings(&self) -> Result<Vec<(String, String)>> {
        let mut settings = Vec::new();
        flatten(&self.0, &mut Vec::new(), &mut settings)?;
        ensure!(
            !settings.is_empty(),
            "a git profile needs at least one setting, such as user.email"
        );
        Ok(settings)
    }
}

fn flatten(
    table: &toml::Table,
    path: &mut Vec<String>,
    settings: &mut Vec<(String, String)>,
) -> Result<()> {
    for (key, value) in table {
        path.push(key.clone());
        let value = match value {
            toml::Value::Table(inner) => {
                flatten(inner, path, settings)?;
                None
            }
            toml::Value::String(text) => Some(text.clone()),
            toml::Value::Integer(number) => Some(number.to_string()),
            toml::Value::Boolean(flag) => Some(flag.to_string()),
            _ => bail!(
                "{}: git settings are strings, integers or booleans",
                path.join(".")
            ),
        };
        if let Some(value) = value {
            ensure!(
                !value.contains('\0'),
                "{}: value contains NUL",
                path.join(".")
            );
            settings.push((config_key(path)?, value));
        }
        path.pop();
    }
    Ok(())
}

/// `section[.subsection].name`, spelled as Git validates it: the first and
/// last components are words, anything between them is the subsection.
fn config_key(path: &[String]) -> Result<String> {
    let key = path.join(".");
    ensure!(
        key.contains('.'),
        "{key}: a git setting is section.name, such as user.email"
    );
    let word = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_alphabetic())
            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    ensure!(
        word(key.split('.').next().unwrap())
            && word(key.rsplit('.').next().unwrap())
            && !key.contains(['\n', '\0']),
        "{key}: git section and setting names are letters, digits and hyphens"
    );
    ensure!(
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "core.worktree" | "core.bare"
        ) && !key.to_ascii_lowercase().starts_with("extensions."),
        "{key}: git profiles cannot change repository layout or extensions"
    );
    Ok(key)
}
