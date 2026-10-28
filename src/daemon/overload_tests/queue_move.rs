//! `shoal queue move` of a running agent stops it, and its wrapper starts the
//! saved session again in the same terminal once the agent queue starts its
//! restore.
use super::*;
use crate::model::QueuePlace;

#[tokio::test]
async fn a_moved_agent_restores_in_its_wrapper_whatever_the_overload_policy() {
    let (_root, manager, workspace, serving) = fixture().await;
    // A protection stop would leave the agent stopped without recovery.
    set_recovery(&manager, false).await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let moved = bounded(manager.queue_move(&workspace.name, QueuePlace::Last, None))
        .await
        .unwrap();
    assert!(moved.stopped);
    assert_eq!(moved.entry.position, Some(1));
    // The same wrapper ran the restore command, which exits at once.
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(queue_is_empty(&manager).await);
    serving.abort();
}
