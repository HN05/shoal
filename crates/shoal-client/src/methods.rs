//! Every request a client sends, by method name, with its parameters and
//! the result it returns. A new method gets a new name; a method's
//! parameters and result only gain fields within a major version.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    open_enum::open_enum,
    records::{Approval, Hold, ItemKind, Question, Relation, Repository, Workspace},
};

/// A request answered with one result, sent with [`crate::Client::call`].
pub trait Method: Serialize {
    /// The method's name on the wire.
    const NAME: &'static str;
    type Output: DeserializeOwned;
}

/// The requests a client sends, and the subscription that streams records.
macro_rules! methods {
    (
        calls { $($(#[$meta:meta])* $wire:literal => $variant:ident -> $output:ty;)+ }
        streams { $($(#[$stream_meta:meta])* $stream_wire:literal => $stream:ident;)+ }
    ) => {
        /// Every request, tagged with its method name.
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        #[serde(tag = "method", content = "params")]
        pub enum Call {
            $($(#[$meta])* #[serde(rename = $wire)] $variant($variant),)+
            $($(#[$stream_meta])* #[serde(rename = $stream_wire)] $stream($stream),)+
        }

        impl Call {
            /// The names of every method this version knows.
            pub const NAMES: &'static [&'static str] = &[$($wire,)+ $($stream_wire,)+];

            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $wire,)+
                    $(Self::$stream(_) => $stream_wire,)+
                }
            }
        }

        $(
            impl Method for $variant {
                const NAME: &'static str = $wire;
                type Output = $output;
            }

            impl From<$variant> for Call {
                fn from(params: $variant) -> Self {
                    Self::$variant(params)
                }
            }
        )+

        $(
            impl From<$stream> for Call {
                fn from(params: $stream) -> Self {
                    Self::$stream(params)
                }
            }
        )+

        /// Each method's parameters and result, for the schema.
        pub(crate) fn describe(generator: &mut schemars::SchemaGenerator) -> serde_json::Value {
            let mut methods = serde_json::Map::new();
            $(
                methods.insert($wire.into(), serde_json::json!({
                    "params": generator.subschema_for::<$variant>(),
                    "result": generator.subschema_for::<$output>(),
                }));
            )+
            $(
                methods.insert($stream_wire.into(), serde_json::json!({
                    "params": generator.subschema_for::<$stream>(),
                    "events": generator.subschema_for::<crate::wire::Event>(),
                }));
            )+
            methods.into()
        }
    };
}

methods! {
    calls {
        /// Every registered repository, most recently used first.
        "repositories.list" => ListRepositories -> Vec<Repository>;
        /// Fetch a repository's default branch from its remote and
        /// fast-forward the local default branch.
        "repository.sync" => SyncRepository -> SyncedRepository;
        "workspace.get" => GetWorkspace -> Workspace;
        /// Remove a workspace as `shoal rm` does, keeping a checkpoint.
        "workspace.remove" => RemoveWorkspace -> Removal;
        /// Record the workspace's assignment as done, as `shoal done` does.
        "workspace.done" => MarkDone -> Completion;
        /// Withdraw a recorded completion, as `shoal done --undo` does.
        "workspace.undo_done" => UndoDone -> ();
        /// Rename the workspace's branch, and the workspace with it.
        "workspace.rename" => RenameWorkspace -> Workspace;
        "link.add" => AddLink -> ();
        "link.remove" => RemoveLink -> ();
        "hold.acquire" => AcquireHold -> Hold;
        "hold.release" => ReleaseHold -> ();
        "simulator.acquire" => AcquireSimulator -> SimulatorLease;
        "simulator.release" => ReleaseSimulator -> ();
        /// Queue an agent in the workspace, which the daemon starts detached
        /// in the default host once the agent queue admits it.
        "agent.queue" => QueueAgent -> QueuedAgent;
        /// Stop what runs in the workspace, saving its agent's session.
        "agent.stop" => StopAgent -> ();
        "question.answer" => AnswerQuestion -> Question;
        /// Approve or deny a request: the user any, a workspace's agent only
        /// those its children's agents passed to it, as with `shoal approvals`.
        "approval.decide" => DecideApproval -> Approval;
    }
    streams {
        /// The records of the topics asked for: a snapshot, then their
        /// changes, until the client hangs up.
        "subscribe" => Subscribe;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListRepositories {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SyncRepository {
    /// The repository's ID, name or path.
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncedRepository {
    pub repository_id: String,
    /// The default branch's upstream remote, absent when it has none.
    pub remote: Option<String>,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetWorkspace {
    /// The workspace's ID or name.
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoveWorkspace {
    pub workspace: String,
    /// Delete uncommitted files and the commits of a detached HEAD. Without
    /// it, a workspace with either is refused with `unsaved_work`.
    #[serde(default)]
    pub discard_unsaved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Removal {
    /// The workspace's branch.
    pub branch: Option<String>,
    /// Whether the branch was deleted; it is kept when only it has some commits.
    pub branch_deleted: bool,
    /// Why the best-effort removal hook failed, after a removal that succeeded.
    pub hook_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkDone {
    pub workspace: String,
    /// Remove the workspace once it qualifies, even where `[cleanup.done] enabled` is false.
    #[serde(default)]
    pub cleanup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Completion {
    /// The commit the completion covers.
    pub head: String,
    /// Whether the workspace is removed once it qualifies.
    pub cleanup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UndoDone {
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameWorkspace {
    pub workspace: String,
    /// The new branch name; the workspace takes the name derived from it.
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddLink {
    pub workspace: String,
    pub kind: ItemKind,
    /// The item's number in the workspace's repository, or its URL.
    pub item: String,
    pub relation: Relation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoveLink {
    pub workspace: String,
    pub kind: ItemKind,
    /// The item's number in the workspace's repository, or its URL.
    pub item: String,
    pub relation: Relation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcquireHold {
    pub workspace: String,
    pub name: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseHold {
    pub workspace: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AcquireSimulator {
    pub workspace: String,
    /// The lease's name; `default` when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// A configured simulator profile.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    /// Why the lease is needed, which a lease that needs approval asks for.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A simulator leased to a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SimulatorLease {
    pub name: String,
    pub udid: Option<String>,
    pub device: String,
    pub runtime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseSimulator {
    pub workspace: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueueAgent {
    pub workspace: String,
    /// The agent to start, such as `claude` or `codex`.
    pub agent: String,
    /// The agent's first message; without one it waits for the user.
    #[serde(default)]
    pub prompt: Option<String>,
}

open_enum! {
    /// Where a queued agent is.
    QueueState {
        /// Waiting for an agent slot or for work it depends on.
        Queued => "queued",
        /// Admitted; its agent has not started yet.
        Starting => "starting",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuedAgent {
    /// The queue entry's ID.
    pub id: String,
    pub state: QueueState,
    /// Its place among waiting entries, from 1; absent once starting.
    pub position: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StopAgent {
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnswerQuestion {
    pub id: i64,
    pub answer: String,
    /// The question as shown. When given, a question that no longer matches
    /// it, such as a later one reusing its number, is refused with `not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DecideApproval {
    pub id: i64,
    pub approve: bool,
    /// The request as shown. When given, a request that no longer matches
    /// it, such as a later one reusing its number, is refused with `not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<Approval>,
}

open_enum! {
    /// What a subscription covers.
    Topic {
        Repositories => "repositories",
        /// Workspaces with their lifecycle state, holds, ready marks and agent.
        Workspaces => "workspaces",
        /// Linked issues and PRs with their forge state.
        Links => "links",
        /// Open questions.
        Questions => "questions",
        /// Pending approval requests.
        Approvals => "approvals",
        /// Messages to agents and from agents to the user.
        Messages => "messages",
        /// The user's notifications; workspace processes cannot subscribe.
        Notifications => "notifications",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Subscribe {
    /// A daemon skips topics it does not know.
    pub topics: Vec<Topic>,
}
