//! What the move tests share.
use anyhow::Result;

use super::Steps;
use crate::{client::connection::CommandPaths, protocol::operations::Step};
use shoal_core::transfer::{Carrying, MoveDestination, MovePhase, MoveRecord};
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::removal::UnsavedWork;
use shoal_protocol::space_names::SpaceSelector;

/// Run the test helper `name` in a process of its own, without the
/// runner's Shoal variables, which would reach the daemon in its scope.
pub(super) async fn isolated(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let output = (crate::test_support::isolated_test(root.path(), name).output())
        .await
        .unwrap();
    let printed = crate::test_support::printed(&output);
    assert!(output.status.success(), "{printed}");
}

/// Whether this process is a helper [`isolated`] runs.
pub(super) fn helper() -> bool {
    std::env::var_os("SHOAL_TEST_HELPER").is_some()
}

/// Steps no one follows.
pub(super) struct Unfollowed;

impl Steps for Unfollowed {
    async fn reached(&self, _step: Step) {}

    fn ensure_running(&self) -> Result<()> {
        Ok(())
    }
}

/// A daemon with a workspace whose move to `destination` wrote its package
/// and was recorded in `phase`, as its worker left it, with the paths that
/// reach the daemon.
pub(super) async fn left_move(
    daemon: &TestDaemon,
    destination: impl FnOnce(&std::path::Path) -> MoveDestination,
) -> (CommandPaths, MoveRecord, std::path::PathBuf) {
    let workspace = daemon.workspace("moving").await;
    let paths = CommandPaths::from(daemon.manager.paths.clone());
    let destination = destination(&daemon.manager.paths.home);
    let space = SpaceSelector::id(&workspace.id);
    let (unsaved, carrying) = (UnsavedWork::default(), Carrying::Subtree);
    let packing = crate::client::transfer::export_moving(
        &paths,
        space,
        unsaved.clone(),
        destination.clone(),
        carrying.clone(),
        false,
    );
    packing.await.unwrap();
    let phase = match destination {
        MoveDestination::Ssh(_)
        | MoveDestination::Machine(_)
        | MoveDestination::Client { .. }
        | MoveDestination::Held => MovePhase::Delivering,
        _ => MovePhase::Delivered,
    };
    let record = MoveRecord {
        space: workspace.id.clone(),
        destination,
        ticket: shoal_ids::move_tickets::MoveTicket::generate(),
        carrying,
        unsaved,
        phase,
        checkpoints: Default::default(),
        saved_before: Default::default(),
    };
    (paths, record, workspace.path.clone())
}
