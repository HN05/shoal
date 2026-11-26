//! The spaces subsystem of the daemon: workspaces, chats and the tree they
//! form. See `docs/design/spaces.md`.
use shoal_daemon_base::forge_access::ForgeAccess;
use shoal_daemon_base::operations::Operations;
use std::sync::Arc;

use shoal_coordination::{alerts, approvals, changes, scope, threads};
use shoal_core::{
    agents, caches, config, daemon_handoff, daemon_owner, env, execution, fetched_copies, fsutil,
    git, git_profile, hooks, paths, presets, process, refusal, removal, repository, service,
    sessions, shoal_dirs, start_plan, terminal, transfer, workers,
};
use shoal_daemon_base::{
    allocation, blockers, config_state::ConfigState, events, file_access, file_access::FileAccess,
    frames, gates, gates::Gates, item_numbers, jobs, log, notifications,
    notifications::Notifications, signals::Signals, stops,
};
use shoal_forge_types as forge;
use shoal_protocol::{agent, links, model, protocol, state, time, validate};
use shoal_store::{self as store, Store};

pub mod absorb;
pub mod carried_commits;
pub mod carried_sessions;
pub mod chat_marks;
pub mod chats;
pub mod cleanup;
pub mod completion_cleanup;
pub mod conversions;
pub mod coordinated;
pub mod daemon;
mod executions;
pub mod holds;
pub mod hook_settings;
pub mod hub_moves;
pub mod imported_sessions;
pub mod launches;
pub mod leftovers;
pub mod lifecycle;
pub mod moves;
pub mod opened_elsewhere;
mod operations;
pub mod ownership;
#[cfg(any(test, feature = "test-support"))]
mod per_daemon;
pub mod pr_gate;
pub mod progress;
pub mod quarantine;
pub mod ready_marks;
pub mod recovery;
pub mod restore;
pub mod space_processes;
pub mod space_removal;
pub mod spaces;
pub mod starts;
pub mod startup_audit;
pub mod target_checks;
pub mod tmp_dirs;
pub mod tree;
pub mod untracked;
pub mod workspace;
pub mod workspace_files;
pub mod workspace_usage;

#[cfg(test)]
mod stand_in;

pub struct Spaces {
    pub store: Store,
    pub gates: Arc<Gates>,
    pub config: Arc<ConfigState>,
    pub notifications: Arc<Notifications>,
    pub signals: Arc<Signals>,
    pub paths: paths::Paths,
    pub file_access: Arc<FileAccess>,
    forges: Arc<ForgeAccess>,
    operations: Arc<Operations>,
    pub cleanup_health: cleanup::CleanupHealth,
    pub idle_sleep_health: cleanup::CleanupHealth,
    /// The spaces the cleanup sweep removed, which it reports.
    pub cleanup_removals: cleanup::Removed,
    pub publications: progress::Publications,
    pub usage: workspace::usage::Usage,
    /// The leftovers of removed spaces that are still being deleted.
    pub leftovers: leftovers::Leftovers,
    pub conversion_handovers: conversions::Handovers,
    /// The spaces whose agent a conversion is moving now.
    pub relocating: conversions::Relocating,
    pub space_removals: spaces::Removing,
    pub completed_owners: spaces::Removing,
    /// The commits workspaces carry for another, held until they land.
    pub carried_holds: carried_commits::CarriedHolds,
    pub records: Arc<dyn daemon::Records>,
}

impl Spaces {
    #[expect(
        clippy::too_many_arguments,
        reason = "what the spaces share with the daemon's other subsystems"
    )]
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        signals: Arc<Signals>,
        paths: paths::Paths,
        file_access: Arc<FileAccess>,
        forges: Arc<ForgeAccess>,
        operations: Arc<Operations>,
        space_removals: spaces::Removing,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            signals,
            paths,
            file_access,
            forges,
            operations,
            cleanup_health: Default::default(),
            idle_sleep_health: cleanup::CleanupHealth::of("Idle sleep"),
            cleanup_removals: Default::default(),
            publications: Default::default(),
            usage: Default::default(),
            leftovers: Default::default(),
            conversion_handovers: Default::default(),
            relocating: Default::default(),
            space_removals,
            completed_owners: Default::default(),
            carried_holds: Default::default(),
            records,
        }
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
