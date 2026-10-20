//! Tokens that user-configured commands print for each forge request; Shoal
//! stores none.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// A `[forgejo."<host>"]` table: how to sign in to one Forgejo or Gitea host.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgejoHost {
    /// Argument array whose standard output is an API token for the host.
    pub token_command: Vec<String>,
}

/// Forgejo and Gitea hosts keyed by the lowercase host, with a port when the
/// repository's remote names one.
pub type ForgejoHosts = BTreeMap<String, ForgejoHost>;

pub fn validate_forgejo(table: &str, hosts: &ForgejoHosts) -> Result<()> {
    for (host, login) in hosts {
        ensure!(
            !host.is_empty()
                && host.chars().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '.' | '-' | ':')),
            // Remote hosts are compared in lowercase.
            "{table}: {host:?} is not a lowercase host name"
        );
        let key = format!("{table}.\"{host}\".token_command");
        ensure!(!login.token_command.is_empty(), "{key} is required");
        validate(&key, &login.token_command)?;
    }
    Ok(())
}

/// A command must name a program and fit an argument array.
pub fn validate(key: &str, command: &[String]) -> Result<()> {
    if let Some(program) = command.first() {
        ensure!(!program.is_empty(), "{key} must name a program");
    }
    ensure!(
        command.iter().all(|word| !word.contains('\0')),
        "{key} contains NUL"
    );
    Ok(())
}

/// Run `command` and return the token it prints. The token goes into a curl
/// config line, so anything but a plain token is refused.
pub(super) async fn fetch(key: &str, command: &[String]) -> Result<String> {
    let (program, args) = command
        .split_first()
        .with_context(|| format!("{key} is empty"))?;
    let mut process = tokio::process::Command::new(program);
    process.args(args);
    let output = crate::subprocess::Run::new(process)
        .timeout(super::api::TIMEOUT)
        .output()
        .await
        .with_context(|| format!("run {key}"))?;
    let token = output.trim();
    ensure!(
        !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\')),
        "{key} printed no token or an unexpected token format"
    );
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(toml: &str) -> ForgejoHosts {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn forgejo_hosts_need_a_host_name_and_a_command() {
        let valid = hosts("[\"forge.example:3000\"]\ntoken_command = ['token']\n");
        assert!(validate_forgejo("forgejo", &valid).is_ok());
        for invalid in [
            "[\"forge.example\"]\n",
            "[\"forge.example\"]\ntoken_command = ['']\n",
            "[\"https://forge.example\"]\ntoken_command = ['token']\n",
            "[\"Forge.example\"]\ntoken_command = ['token']\n",
        ] {
            assert!(
                validate_forgejo("forgejo", &hosts(invalid)).is_err(),
                "{invalid}"
            );
        }
        assert!(toml::from_str::<ForgejoHosts>("[h]\ntoken = 'literal'\n").is_err());
    }

    #[tokio::test]
    async fn fetched_tokens_are_trimmed_and_refused_when_malformed() {
        let command = |text: &str| vec!["printf".to_owned(), text.to_owned()];
        assert_eq!(fetch("key", &command("abc123\n")).await.unwrap(), "abc123");
        for bad in ["", "a\"b", "a b"] {
            assert!(fetch("key", &command(bad)).await.is_err(), "{bad:?}");
        }
        let error = fetch("key", &["false".to_owned()]).await.unwrap_err();
        assert!(format!("{error:#}").contains("run key"), "{error:#}");
    }
}
