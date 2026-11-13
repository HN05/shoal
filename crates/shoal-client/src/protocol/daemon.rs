use serde::{Deserialize, Serialize};

use super::operations::OperationKind;
use crate::space_ids::{SpaceId, SpaceNumber};
use crate::space_names::SpaceName;
use crate::state::WorkspaceState;

/// Daemon administration, which must reach a daemon of any release.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMethod {
    Status,
    ReloadConfig,
}

/// Ask the daemon to exit. Daemons answer it before their protocol check, so
/// service commands reach a daemon of another release; keep its spelling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExitRequest {
    pub exit: Exit,
    #[serde(default)]
    pub when: ExitWhen,
}

/// When the daemon exits, against work in progress that its exit would cut
/// off. A daemon that waits sends the work it waits for as
/// [`super::Body::ExitWaits`], again each time it changes, before `ok`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitWhen {
    /// At once, whatever runs.
    #[default]
    Now,
    /// Once that work has ended, holding back new work meanwhile.
    Idle,
    /// At once when no such work runs; otherwise not at all, after sending
    /// the work that runs.
    IfIdle,
}

/// Work in progress that the daemon's exit would cut off. Agents and CI
/// jobs reattach to the next daemon, so none of them is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Work {
    /// A workspace in a state its exit would interrupt, such as preparing
    /// or removing.
    Workspace {
        workspace: WorkSpace,
        state: WorkspaceState,
    },
    /// An operation's worker, such as a PR's push or a merge.
    Operation {
        workspace: WorkSpace,
        operation: OperationKind,
    },
    /// A command in the workspace whose wrapper stops with the daemon, such
    /// as `shoal setup`.
    Command { workspace: WorkSpace },
    /// Requests and background passes, such as a cleanup sweep or a sync of
    /// a checkout, when nothing else runs.
    Other,
}

impl Work {
    /// Whether `work` refuses an exit that waits only when nothing but
    /// requests and background passes runs, which end soon on their own.
    pub fn refuses_idle_exit(work: &[Self]) -> bool {
        work.iter().any(|work| !matches!(work, Self::Other))
    }
}

/// The workspace a piece of [`Work`] works in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSpace {
    pub id: SpaceId,
    pub number: SpaceNumber,
    pub name: SpaceName,
}

/// What happens to running work as the daemon exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// Work stops as `shoal stop --all` stops it.
    Stop,
    /// Work keeps running and reattaches to the next daemon.
    Restart,
}

/// What a daemon an exit request ended left, which it sends after its
/// answer to the request once its work has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exited {
    /// Stopped work waits for `shoal restore`.
    pub stopped_work: bool,
}
