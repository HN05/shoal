//! Named Git identities, written into a worktree's own Git config so the
//! shared repository config and the other worktrees keep theirs.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::{git, validate};

/// Global `[git]`: the profiles a repository config or `add --git-profile`
/// may select by name.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
                    "git profile {name} is not defined; add [git.profiles.{name}] to the global config"
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

/// Write `profile` into the worktree's own config. The first profile in a
/// repository enables Git's `worktreeConfig` extension in the shared config;
/// a shared `core.worktree` or `core.bare = true` would then break the main
/// checkout unless migrated to its config.worktree, so those are refused.
pub async fn apply(worktree: &Path, profile: &Profile) -> Result<()> {
    let settings = profile.settings()?;
    let shared = |key: &'static str, kind: &'static str| async move {
        git::run_isolated(
            worktree,
            &[
                "config",
                "--local",
                &format!("--type={kind}"),
                "--default",
                if kind == "bool" { "false" } else { "" },
                "--get",
                key,
            ],
        )
        .await
        .map(|value| value.trim().to_owned())
    };
    if shared("extensions.worktreeConfig", "bool").await? != "true" {
        ensure!(
            shared("core.worktree", "path").await?.is_empty()
                && shared("core.bare", "bool").await? != "true",
            "the repository's shared Git config sets core.worktree or core.bare; move them to the main worktree's config.worktree before using git profiles"
        );
        git::run_isolated(
            worktree,
            &["config", "--local", "extensions.worktreeConfig", "true"],
        )
        .await?;
    }
    for (key, value) in &settings {
        git::run_isolated(worktree, &["config", "--worktree", key, value]).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(text: &str) -> Result<Vec<(String, String)>> {
        toml::from_str::<Profile>(text)?.settings()
    }

    #[test]
    fn settings_flatten_dotted_keys_and_tables_in_git_spelling() {
        let settings = profile(
            "user.name = 'Name'\ncommit.gpgsign = true\n[gpg]\nformat = 'ssh'\n\
             [url.'https://x/']\ninsteadOf = 'git@x:'\n[http]\npostBuffer = 5\n",
        )
        .unwrap();
        assert_eq!(
            settings,
            [
                ("commit.gpgsign", "true"),
                ("gpg.format", "ssh"),
                ("http.postBuffer", "5"),
                ("url.https://x/.insteadOf", "git@x:"),
                ("user.name", "Name"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
        for text in [
            "",
            "user = 'flat'",
            "user.email = 1.5",
            "user.email = ['a']",
            "'bad section'.email = 'x'",
            "user.'bad name' = 'x'",
            "user.-name = 'x'",
            "core.worktree = '/elsewhere'",
            "core.bare = true",
            "extensions.worktreeConfig = false",
            "user.email = \"a\\u0000b\"",
        ] {
            assert!(profile(text).is_err(), "{text}");
        }
    }

    #[tokio::test]
    async fn incompatible_shared_layout_is_not_changed() {
        let root = tempfile::tempdir().unwrap();
        git::run(root.path(), &["init", "-b", "main"])
            .await
            .unwrap();
        let profile: Profile = toml::from_str("user.email = 'work@example.invalid'").unwrap();
        for (key, value) in [("core.bare", "true"), ("core.worktree", "/elsewhere")] {
            git::run(root.path(), &["config", "--local", key, value])
                .await
                .unwrap();
            assert!(
                apply(root.path(), &profile)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("shared Git config")
            );
            assert_eq!(
                git::run(
                    root.path(),
                    &[
                        "config",
                        "--local",
                        "--default",
                        "false",
                        "--get",
                        "extensions.worktreeConfig"
                    ]
                )
                .await
                .unwrap()
                .trim(),
                "false"
            );
            assert!(!root.path().join(".git/config.worktree").exists());
            git::run(root.path(), &["config", "--local", "--unset", key])
                .await
                .unwrap();
        }
    }

    #[test]
    fn profiles_are_looked_up_by_portable_name() {
        let git: Git = toml::from_str(
            "[profiles.work]\nuser.email = 'w@x'\n[profiles.oss]\nuser.email = 'o@x'\n",
        )
        .unwrap();
        git.validate().unwrap();
        assert_eq!(git.profile("work").unwrap().settings().unwrap()[0].1, "w@x");
        let error = git.profile("home").unwrap_err().to_string();
        assert!(error.contains("oss, work"), "{error}");
        assert!(
            Git::default()
                .profile("home")
                .unwrap_err()
                .to_string()
                .contains("[git.profiles.home]")
        );
        for text in [
            "[profiles.'bad name']\nuser.email = 'x'\n",
            "[profiles.empty]\n",
            "[profile.work]\nuser.email = 'x'\n",
        ] {
            let parsed = toml::from_str::<Git>(text)
                .map_err(anyhow::Error::from)
                .and_then(|git| git.validate());
            assert!(parsed.is_err(), "{text}");
        }
    }
}
