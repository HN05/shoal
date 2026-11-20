use crate::moves::fixtures::{Unfollowed, helper, isolated, left_move};
use crate::protocol::ErrorCode;
use crate::protocol::RemoteError;
use shoal_core::transfer::{Carrying, Consent, MoveDestination, MoveOrder, SshDestination};
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::removal::UnsavedWork;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_move_of_a_space_another_move_left_leaves_that_move_alone() {
    isolated(
        "moves::moving::tests::a_move_of_a_space_another_move_left_leaves_that_move_alone_child",
    )
    .await;
}

#[tokio::test]
#[ignore = "isolated helper of a_move_of_a_space_another_move_left_leaves_that_move_alone"]
async fn a_move_of_a_space_another_move_left_leaves_that_move_alone_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    // A move over SSH that stopped while the other machine imported it.
    let ssh = |_: &std::path::Path| {
        MoveDestination::Ssh(SshDestination {
            host: "mini".into(),
            shoal: None,
            resume: true,
        })
    };
    let (paths, left, path) = left_move(&daemon, ssh).await;
    let destination = MoveDestination::File {
        path: daemon.manager.paths.home.join("again.shoal"),
    };
    let order = MoveOrder {
        spaces: vec![left.space.clone()],
        destination: destination.clone(),
        recursive: true,
        keep: false,
        checkpoint: None,
        consent: Some(Consent::Any),
    };

    let report = crate::moves::run(&paths, order, &Unfollowed).await.unwrap();
    assert!(
        report.moved.is_empty() && report.failed.len() == 1,
        "{report:?}"
    );
    assert!(path.exists());
    // The move it left is still recorded, so another is refused as before.
    let space = SpaceSelector::id(&left.space);
    let (unsaved, carrying) = (UnsavedWork::default(), Carrying::Subtree);
    let again = crate::client::transfer::export_moving(
        &paths,
        space,
        unsaved,
        destination,
        carrying,
        false,
    );
    let refused = again.await.unwrap_err();
    let code = refused
        .downcast_ref::<RemoteError>()
        .map(|refusal| refusal.code.clone());
    assert_eq!(code, Some(ErrorCode::MoveInProgress));
}
