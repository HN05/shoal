use crate::{
    daemon::events::{EventCause, EventItem, EventKind},
    model::WorkspaceRef,
    removal::{BranchChoice, InspectionPolicy},
    test_support::{manager, repository},
};

fn reference(workspace: &crate::model::Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

#[tokio::test]
async fn creation_infers_the_base_and_removal_moves_stacked_workspaces_down() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str, base: Option<&str>| {
        let manager = manager.clone();
        let (repo, name, base) = (repo.id.clone(), name.to_owned(), base.map(str::to_owned));
        async move {
            manager
                .create_workspace(&repo, name, base, None, None)
                .await
                .unwrap()
        }
    };
    let lower = create("lower", None).await;
    let middle = create("middle", Some("lower")).await;
    let upper = create("upper", Some("refs/heads/middle")).await;
    let commit = create("commit", Some(&lower.base_commit.clone().unwrap())).await;
    assert_eq!(middle.base_workspace, Some(reference(&lower)));
    assert_eq!(upper.base_workspace, Some(reference(&middle)));
    assert_eq!(lower.base_workspace, None);
    assert_eq!(commit.base_workspace, None);
    assert_eq!(
        manager
            .workspace("middle")
            .await
            .unwrap()
            .stacked_workspaces,
        [reference(&upper)]
    );

    for (removed, base) in [(&middle, Some(reference(&lower))), (&lower, None)] {
        manager
            .remove_workspace(&removed.id, BranchChoice::Auto, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        assert_eq!(
            manager.workspace("upper").await.unwrap().base_workspace,
            base
        );
    }

    let events = manager.workspace_events(None, 100).await.unwrap();
    let bases: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event)
                if event.details.workspace_id == upper.id
                    && matches!(
                        event.details.kind,
                        EventKind::Created | EventKind::BaseChanged
                    ) =>
            {
                Some((
                    event.details.kind,
                    event.details.cause,
                    event.details.base_workspace.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        bases,
        [
            (EventKind::Created, None, Some(Some(reference(&middle)))),
            (
                EventKind::BaseChanged,
                Some(EventCause::Removed),
                Some(Some(reference(&lower)))
            ),
            (
                EventKind::BaseChanged,
                Some(EventCause::Removed),
                Some(None)
            ),
        ]
    );
}
