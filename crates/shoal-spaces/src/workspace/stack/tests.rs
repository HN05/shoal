use crate::model::EventCause;
use crate::stand_in::Fixture;
use crate::{
    events::{EventItem, EventKind, SpaceEvent},
    model::WorkspaceRef,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_core::test_support::repository;
use shoal_protocol::space_names::SpaceSelector;

fn reference(workspace: &crate::model::Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch().cloned(),
    }
}

#[tokio::test]
async fn creation_infers_the_base_and_removal_moves_stacked_workspaces_down() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str, base: Option<&str>| {
        let manager = manager.daemon.clone();
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
    let commit = create(
        "commit",
        Some(lower.checkout.base().unwrap().commit.as_str()),
    )
    .await;
    assert_eq!(middle.base_workspace, Some(reference(&lower)));
    assert_eq!(upper.base_workspace, Some(reference(&middle)));
    assert_eq!(lower.base_workspace, None);
    assert_eq!(commit.base_workspace, None);
    assert_eq!(
        manager
            .spaces
            .workspace(&manager.named("middle").await)
            .await
            .unwrap()
            .stacked_workspaces,
        [reference(&upper)]
    );

    for (removed, base) in [(&middle, Some(reference(&lower))), (&lower, None)] {
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&removed.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(
            manager
                .spaces
                .workspace(&manager.named("upper").await)
                .await
                .unwrap()
                .base_workspace,
            base
        );
    }

    let events = manager.space_events(None, 100).await.unwrap();
    let bases: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event)
                if event.details.space_id == upper.id
                    && matches!(
                        event.details.kind(),
                        EventKind::Created | EventKind::BaseChanged
                    ) =>
            {
                Some((
                    event.details.kind(),
                    event.details.cause,
                    match &event.details.event {
                        SpaceEvent::Created { base_workspace, .. }
                        | SpaceEvent::BaseChanged { base_workspace } => base_workspace.clone(),
                        _ => None,
                    },
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        bases,
        [
            (EventKind::Created, None, Some(reference(&middle))),
            (
                EventKind::BaseChanged,
                Some(EventCause::Removed),
                Some(reference(&lower))
            ),
            (EventKind::BaseChanged, Some(EventCause::Removed), None),
        ]
    );
}
