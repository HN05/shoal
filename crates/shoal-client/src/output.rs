//! What CLI commands print with `--json`: each command prints one of these
//! types or a type of the protocol, so a Rust program that runs `shoal
//! --json` parses its output with Shoal's own types.
use serde::{Serialize, de::DeserializeOwned};

pub mod acquisition;
pub mod agents;
pub mod alerts;
pub mod batch;
pub mod catalog;
pub mod cleanup;
pub mod config;
pub mod configured;
pub mod convert;
pub mod create;
pub mod daemon;
pub mod demo;
pub mod download;
pub mod error;
pub mod forge;
pub mod hub;
pub mod inbox;
pub mod items;
pub mod leases;
pub mod overviews;
pub mod removal;
pub mod repositories;
pub mod restore;
pub mod resume;
pub mod shell;
pub mod skill;
pub mod spaces;
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

/// What a command prints for `value`: it as JSON when `json`, otherwise
/// `message`.
pub fn line<T: Output + ?Sized>(
    json: bool,
    message: &str,
    value: &T,
) -> serde_json::Result<String> {
    match json {
        true => self::json(value),
        false => Ok(message.to_owned()),
    }
}

outputs!(
    (),
    bool,
    String,
    std::collections::BTreeMap<String, String>,
    shoal_protocol::approvals::ApprovalRequest,
    shoal_protocol::branches::DeletedBranch,
    shoal_protocol::branches::ListedBranch,
    shoal_protocol::containers::ContainerOverview,
    shoal_protocol::containers::Containers,
    shoal_protocol::demos::Running,
    shoal_protocol::events::EventItem,
    shoal_protocol::hub::directory::Listed,
    shoal_protocol::status::ReportedStatus,
    shoal_protocol::files::AttachedFile,
    shoal_protocol::log::LogEntry,
    shoal_protocol::links::Proposed,
    shoal_protocol::links::Updates,
    shoal_protocol::alerts::Cleared,
    shoal_protocol::alerts::Alert,
    shoal_protocol::alerts::Sent,
    shoal_protocol::progress::Progress,
    shoal_protocol::threads::Listing,
    shoal_protocol::threads::Thread,
    shoal_protocol::chat_marks::ChatMarked,
    shoal_protocol::changes::Change,
    shoal_protocol::changes::ChangeDiff,
    shoal_protocol::resources::Overview,
    shoal_protocol::resources::ResourceLease,
    shoal_protocol::reviews::Review,
    shoal_protocol::secrets::SecretGrant,
    shoal_protocol::sim::AuditEntry,
    shoal_protocol::sim::Simulator,
    shoal_protocol::sim::SimulatorOverview,
    shoal_protocol::sleep::Slept,
    shoal_protocol::triggers::Fired,
    shoal_protocol::triggers::TriggerRun,
    shoal_protocol::triggers::TriggerStatus,
    shoal_protocol::priority::Prioritized,
    shoal_protocol::model::AgentStatus,
    shoal_protocol::model::Chat,
    shoal_protocol::model::Checkpoint,
    shoal_protocol::model::Completion,
    shoal_protocol::model::ConflictCheck,
    shoal_protocol::model::MergedBranch,
    shoal_protocol::model::Unmerged,
    shoal_protocol::model::LocalConfigEdit,
    shoal_protocol::model::MovedRepository,
    shoal_protocol::model::PortOverview,
    shoal_protocol::model::PortReservation,
    shoal_protocol::model::QueueEntry,
    shoal_protocol::model::RemovedSpace,
    shoal_protocol::model::Repository,
    shoal_protocol::model::RepositoryRemoval,
    shoal_protocol::model::RestoredCheckpoint,
    shoal_protocol::model::Refreshed,
    shoal_protocol::model::ReviewMark,
    shoal_protocol::model::SyncedRepository,
    shoal_protocol::model::Workspace,
    shoal_protocol::model::Space,
    shoal_protocol::model::SpaceHold,
    shoal_protocol::alerts::Reached,
    shoal_protocol::protocol::Launched,
    shoal_protocol::config::options::Entry,
    shoal_protocol::service::Definition,
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
