//! What CLI commands print with `--json`: each command prints one of these
//! types or a type of the protocol, so a Rust program that runs `shoal
//! --json` parses its output with Shoal's own types.
use serde::{Serialize, de::DeserializeOwned};

pub mod acquisition;
pub mod agents;
pub mod batch;
pub mod catalog;
pub mod chat;
pub mod cleanup;
pub mod config;
pub mod configured;
pub mod daemon;
pub mod demo;
pub mod download;
pub mod error;
pub mod forge;
pub mod inbox;
pub mod items;
pub mod leases;
pub mod messages;
pub mod overviews;
pub mod repositories;
pub mod resume;
pub mod shell;
pub mod skill;
pub mod split;
pub mod status;
pub mod transfer;
pub mod update;
pub mod view;
pub mod workspaces;

/// A type a command prints with `--json`, which reads back as `Owned`.
pub trait Output: Serialize {
    /// The type the printed JSON reads back as.
    type Owned: Serialize + DeserializeOwned;
}

impl<T: Output + ?Sized> Output for &T {
    type Owned = T::Owned;
}

impl<T: Output> Output for [T] {
    type Owned = Vec<T::Owned>;
}

impl<T: Output> Output for Vec<T> {
    type Owned = Vec<T::Owned>;
}

impl<T: Output> Output for Option<T> {
    type Owned = Option<T::Owned>;
}

impl<T: Output> Output for Box<T> {
    type Owned = Box<T::Owned>;
}

/// Mark each of the types as one a command prints.
#[macro_export]
macro_rules! outputs {
    ($($output:ty),+ $(,)?) => {$(
        impl $crate::output::Output for $output {
            type Owned = $output;
        }
    )+};
}
pub use outputs;

/// `value` as one line of JSON. A development build checks that it reads
/// back as its type, as a program reading the output parses it.
pub fn json<T: Output + ?Sized>(value: &T) -> serde_json::Result<String> {
    let text = serde_json::to_string(value)?;
    if cfg!(debug_assertions) {
        let parsed: T::Owned = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("output does not read back: {error}: {text}"));
        assert_eq!(
            serde_json::to_value(&parsed)?,
            serde_json::from_str::<serde_json::Value>(&text)?,
            "output reads back as other JSON"
        );
    }
    Ok(text)
}

outputs!(
    (),
    bool,
    String,
    std::collections::BTreeMap<String, String>,
    crate::agent_messages::AgentMessage,
    crate::approvals::ApprovalRequest,
    crate::demos::Running,
    crate::events::EventItem,
    crate::files::AttachedFile,
    crate::log::LogEntry,
    crate::links::Proposed,
    crate::links::Updates,
    crate::notifications::Cleared,
    crate::notifications::Notification,
    crate::notifications::Sent,
    crate::progress::Progress,
    crate::questions::Listing,
    crate::questions::Question,
    crate::resources::Overview,
    crate::resources::WorkspaceOverview,
    crate::resources::ResourceLease,
    crate::reviews::Review,
    crate::secrets::SecretGrant,
    crate::sim::AuditEntry,
    crate::sim::Simulator,
    crate::sim::SimulatorOverview,
    crate::model::AgentStatus,
    crate::model::Chat,
    crate::model::Checkpoint,
    crate::model::Completion,
    crate::model::ConflictCheck,
    crate::model::MergedBranch,
    crate::model::Unmerged,
    crate::model::LocalConfigEdit,
    crate::model::MovedEntry,
    crate::model::MovedRepository,
    crate::model::PortOverview,
    crate::model::PortReservation,
    crate::model::QueueEntry,
    crate::model::RemovedWorkspace,
    crate::model::Repository,
    crate::model::RepositoryRemoval,
    crate::model::RestoredCheckpoint,
    crate::model::ReviewMark,
    crate::model::SyncedRepository,
    crate::model::Workspace,
    crate::model::WorkspaceHold,
    crate::forge::capability::Capabilities,
    crate::forge::ci::CiActed,
    crate::forge::ci::approval::RunsApproved,
    crate::forge::ci::checks::CheckLog,
    crate::forge::ci::run::StartedRun,
    crate::forge::ci::run_action::Run,
    crate::forge::ci::run_action::RunActed,
    crate::forge::ci::run_view::RunView,
    crate::forge::conventions::labels::Label,
    crate::forge::items::item::Item,
    crate::forge::items::item::Opened,
    crate::forge::items::reaction::allowed::AllowedReactions,
    crate::forge::review::reviewers::Reviewer,
);
