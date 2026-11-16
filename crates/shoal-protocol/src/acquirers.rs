//! Who acquired a simulator lease or resource permit, which decides how long
//! it lasts: the user's stays until released, an execution's ends with it.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

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
    /// The agent execution of a scoped caller, whose lease ends when the
    /// agent exits or is stopped.
    Agent { execution_id: ExecutionId },
    /// A demo's execution, whose lease ends when the demo does.
    Demo { execution_id: ExecutionId },
}

impl Acquirer {
    /// The acquirer stored as `kind` and `execution`, which only the user
    /// goes without.
    pub fn from_parts(kind: AcquirerKind, execution: Option<ExecutionId>) -> Result<Self> {
        Ok(match (kind, execution) {
            (AcquirerKind::User, None) => Self::User,
            (AcquirerKind::Agent, Some(execution_id)) => Self::Agent { execution_id },
            (AcquirerKind::Demo, Some(execution_id)) => Self::Demo { execution_id },
            (AcquirerKind::User, Some(_)) => bail!("a lease the user acquired names an execution"),
            (kind, None) => bail!("a lease acquired by {kind} names no execution"),
        })
    }

    pub fn kind(&self) -> AcquirerKind {
        match self {
            Self::User => AcquirerKind::User,
            Self::Agent { .. } => AcquirerKind::Agent,
            Self::Demo { .. } => AcquirerKind::Demo,
        }
    }

    /// The execution whose end ends the lease, none for the user's.
    pub fn execution(&self) -> Option<&ExecutionId> {
        match self {
            Self::User => None,
            Self::Agent { execution_id } | Self::Demo { execution_id } => Some(execution_id),
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
