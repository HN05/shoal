use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    scope::{Caller, authorize},
    test_support::{manager, repository, workspace_below},
};
use crate::protocol::{AgentMethod, Operation, WorkspaceMethod};

fn caller(space: &SpaceId, read_only: bool) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only,
    }
}

/// A read-only scope reads the spaces below its own, and is refused what
/// the same scope may otherwise do there: a read that changes the agent's
/// watch, and a change.
#[tokio::test]
async fn read_only_scope_reads_without_changing() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repository = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repository.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = workspace_below(&manager, &repository, "child", &parent.id, None).await;
    let operations = |space: &SpaceId| {
        let space = SpaceSelector::id(space);
        [
            Operation::Workspace(WorkspaceMethod::SpaceStatus {
                space: space.clone(),
            }),
            Operation::Agent(AgentMethod::AgentUpdates {
                space: space.clone(),
            }),
            Operation::Agent(AgentMethod::MarkAgentMessagesDelivered {
                space,
                ids: Vec::new(),
            }),
        ]
    };
    for read_only in [false, true] {
        let token = manager.operations.issue(&caller(&parent.id, read_only));
        let mut allowed = Vec::new();
        for mut operation in operations(&child.id) {
            let authorized = authorize(&manager, Some(&token), &mut operation).await;
            allowed.push(authorized.is_ok());
        }
        let expected = match read_only {
            false => [true, true, true],
            true => [true, false, false],
        };
        assert_eq!(allowed, expected, "read_only: {read_only}");
    }
}
