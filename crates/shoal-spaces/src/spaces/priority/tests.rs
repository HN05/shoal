use crate::spaces::priority::*;
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use shoal_schedule::priority::Priority;

/// A workspace at a commit records the priority its creation asks for.
#[tokio::test]
async fn a_workspace_at_a_commit_records_the_priority_it_is_given() {
    use shoal_protocol::protocol::{CommitStart, CreationOptions};
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let options = CreationOptions {
        priority: Some(Priority::URGENT),
        ..Default::default()
    };

    let workspace = manager
        .spaces
        .create_commit_workspace(
            &*manager,
            (&repo.id).into(),
            (None, None),
            CommitStart::Revision(None),
            options,
        )
        .await
        .unwrap();

    let id = workspace.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}
