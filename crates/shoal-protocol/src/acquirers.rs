//! Who acquired a simulator lease, containers or a resource permit, which
//! decides how long it lasts: the user's stays until released, a space's
//! agent's until that agent ends for good, and a demo's ends with it.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

shoal_states::states!(
    /// What acquired a lease, as its SQLite column and listings name it.
    AcquirerKind {
        User => "user",
        Agent => "agent",
        Demo => "demo",
    }
);

/// Who acquired a lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Acquirer {
    /// An unscoped caller, whose lease stays until it is released or its
    /// workspace is removed.
    User,
    /// The agent of the space `space_id`, a scoped caller's own or its
    /// coordinator's, whose lease outlives its sleeps, restores and restarts
    /// and ends when that agent quits or is stopped.
    Agent { space_id: SpaceId },
    /// A demo's execution, whose lease ends when the demo does.
    Demo { execution_id: ExecutionId },
}

impl Acquirer {
    /// The acquirer stored as `kind` and `id`, an agent's space or a demo's
    /// execution, which only the user goes without.
    pub fn from_parts(kind: AcquirerKind, id: Option<String>) -> Result<Self> {
        Ok(match (kind, id) {
            (AcquirerKind::User, None) => Self::User,
            (AcquirerKind::Agent, Some(id)) => Self::Agent {
                space_id: id.into(),
            },
            (AcquirerKind::Demo, Some(id)) => Self::Demo {
                execution_id: id.into(),
            },
            (AcquirerKind::User, Some(_)) => bail!("a lease the user acquired names an acquirer"),
            (kind, None) => bail!("a lease acquired by {kind} names no {kind}"),
        })
    }

    /// The ID stored beside its kind: the agent's space or the demo's
    /// execution, none for the user.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::User => None,
            Self::Agent { space_id } => Some(space_id.as_str()),
            Self::Demo { execution_id } => Some(execution_id.as_str()),
        }
    }

    pub fn kind(&self) -> AcquirerKind {
        match self {
            Self::User => AcquirerKind::User,
            Self::Agent { .. } => AcquirerKind::Agent,
            Self::Demo { .. } => AcquirerKind::Demo,
        }
    }

    /// The space whose agent acquired the lease, none for another acquirer.
    pub fn agent_space(&self) -> Option<&SpaceId> {
        match self {
            Self::Agent { space_id } => Some(space_id),
            _ => None,
        }
    }

    /// The acquirer after `requested` acquires the same lease again: the
    /// user's acquire makes it the user's, and any other leaves it as it is.
    pub fn renewed_by(&self, requested: &Acquirer) -> Acquirer {
        match requested {
            Self::User => Self::User,
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
