//! `[gate]`: the agent that decides the approval requests of a permission
//! set to `gate`, what it is told besides its built-in instructions, and the
//! cases it passes to the user; `[gate.KEY]` adds to both for the requests
//! that key governs.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::config::{options::Options, permissions::PermissionKey};

#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Gate)]
pub struct GateConfig {
    /// The agent that decides; unset, `agent.default`.
    #[serde(deserialize_with = "crate::agent::configured")]
    pub agent: Option<Agent>,
    /// The agent that decides once more when the first one fails.
    #[serde(deserialize_with = "crate::agent::configured")]
    pub fallback_agent: Option<Agent>,
    /// What the gate is told after its built-in instructions, such as the
    /// repository's contributor rules.
    pub instructions: Option<String>,
    /// The cases the gate passes to the user, each of which an escalation
    /// quotes word for word.
    #[option(default = Vec::new())]
    pub escalate: Option<Vec<String>>,
    #[option(section)]
    pub issue_open: GateKeyConfig,
    #[option(section)]
    pub pr_open: GateKeyConfig,
    #[option(section)]
    pub issue_other_repositories: GateKeyConfig,
    #[option(section)]
    pub merge: GateKeyConfig,
    #[option(section)]
    pub push: GateKeyConfig,
}

/// What `[gate.KEY]` adds to `[gate]` for the requests the key governs.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = GateKey)]
pub struct GateKeyConfig {
    pub instructions: Option<String>,
    #[option(default = Vec::new())]
    pub escalate: Option<Vec<String>>,
}

impl GateConfig {
    /// Refuse an escalation case without text, which no verdict could quote.
    pub fn validate(&self) -> Result<()> {
        let tables = [
            ("gate", &self.escalate),
            ("gate.issue_open", &self.issue_open.escalate),
            ("gate.pr_open", &self.pr_open.escalate),
            (
                "gate.issue_other_repositories",
                &self.issue_other_repositories.escalate,
            ),
            ("gate.merge", &self.merge.escalate),
            ("gate.push", &self.push.escalate),
        ];
        for (table, cases) in tables {
            ensure!(
                cases.iter().flatten().all(|case| !case.trim().is_empty()),
                "{table}.escalate: a case needs text"
            );
        }
        Ok(())
    }
}

/// What the gate is told for a request the keys `governing` govern.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateRules {
    /// `[gate]`'s instructions, then each governing key's.
    pub instructions: Vec<String>,
    /// `[gate]`'s cases, then each governing key's, without repeats.
    pub escalate: Vec<String>,
}

impl Gate {
    /// The rules for a request the keys `governing` govern.
    pub fn rules(&self, governing: &[PermissionKey]) -> GateRules {
        let keys = governing.iter().filter_map(|&key| self.key(key));
        let tables = std::iter::once((&self.instructions, &self.escalate))
            .chain(keys.map(|key| (&key.instructions, &key.escalate)));
        let mut rules = GateRules::default();
        for (instructions, escalate) in tables {
            rules.instructions.extend(instructions.clone());
            for case in escalate {
                if !rules.escalate.contains(case) {
                    rules.escalate.push(case.clone());
                }
            }
        }
        rules
    }

    /// `[gate.KEY]` for `key`; a key whose values have no `gate` has none.
    fn key(&self, key: PermissionKey) -> Option<&GateKey> {
        match key {
            PermissionKey::IssueOpen => Some(&self.issue_open),
            PermissionKey::PrOpen => Some(&self.pr_open),
            PermissionKey::IssueOtherRepositories => Some(&self.issue_other_repositories),
            PermissionKey::Merge => Some(&self.merge),
            PermissionKey::Push => Some(&self.push),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
