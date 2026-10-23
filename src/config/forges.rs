//! `[forges."<host>"]`: what a forge host serves, set instead of asking it.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::Deserialize;

use crate::forge::backends::ForgeKind;

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Forges(BTreeMap<String, Forge>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forge {
    /// The forge's API, pinned so the host is never asked.
    #[serde(rename = "type")]
    pub kind: ForgeKind,
}

impl Forges {
    /// The type set for `host`, a lowercase host as remotes name it.
    pub fn pinned(&self, host: &str) -> Option<ForgeKind> {
        self.0
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(host))
            .map(|(_, forge)| forge.kind)
    }

    /// The type set for `host`, else its well-known one.
    pub fn settled(&self, host: &str) -> Option<ForgeKind> {
        self.pinned(host).or_else(|| ForgeKind::named(host))
    }

    /// Each pinned host and its type.
    pub fn pins(&self) -> Vec<(String, ForgeKind)> {
        self.0
            .iter()
            .map(|(host, forge)| (host.clone(), forge.kind))
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        for host in self.0.keys() {
            ensure!(
                !host.is_empty()
                    && host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')),
                "forges.{host:?} must name a host, such as \"git.example.com\" or \"git.example.com:3000\""
            );
        }
        Ok(())
    }
}

/// The setting that pins `host`'s type, for messages that ask for it.
pub fn setting(host: &str) -> String {
    format!(
        "set its type to \"github\", \"gitlab\", \"forgejo\" or \"bitbucket\" under [forges.\"{host}\"] in the global config"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_pin_a_known_type_case_insensitively() {
        let forges: Forges = toml::from_str("[\"Git.Example.com\"]\ntype = \"gitlab\"\n").unwrap();
        forges.validate().unwrap();
        assert_eq!(forges.pinned("git.example.com"), Some(ForgeKind::GitLab));
        assert_eq!(forges.pinned("other.example.com"), None);
        assert!(toml::from_str::<Forges>("[\"git.example.com\"]\ntype = \"svn\"\n").is_err());
        assert!(toml::from_str::<Forges>("[\"git.example.com\"]\nkind = \"github\"\n").is_err());
        let url: Forges =
            toml::from_str("[\"https://git.example.com\"]\ntype = \"github\"\n").unwrap();
        assert!(url.validate().is_err());
    }
}
