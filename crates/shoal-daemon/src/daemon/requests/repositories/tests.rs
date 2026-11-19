//! Repository requests from a scoped caller.
use super::*;
use crate::daemon::test_support::manager;
use shoal_ids::space_ids::SpaceId;

/// A process here cannot carry commits into a repository, which would
/// register its origin; only another machine's start does.
#[tokio::test]
async fn a_scoped_process_here_carries_no_commits() {
    let (_temp, manager) = manager().await;
    let caller = Caller {
        space_id: SpaceId::generate(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let mut method = RepositoryMethod::ReceiveCommits {
        origin: "https://example.test/team/app".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        branch: "main".into(),
    };
    assert!(method.scope(&manager, &caller).await.is_err());
    assert!(manager.repositories().await.unwrap().is_empty());
}
