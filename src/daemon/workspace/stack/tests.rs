use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{EventCause, EventItem, EventKind},
        test_support::{manager, repository},
    },
    model::WorkspaceRef,
    removal::{InspectionPolicy, UnsavedWork},
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
            .workspace(&"middle".into())
            .await
            .unwrap()
            .stacked_workspaces,
        [reference(&upper)]
    );

    for (removed, base) in [(&middle, Some(reference(&lower))), (&lower, None)] {
        manager
            .remove_workspace(
                &SpaceSelector::id(&removed.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(
            manager
                .workspace(&"upper".into())
                .await
                .unwrap()
                .base_workspace,
            base
        );
    }

    let events = manager.workspace_events(None, 100).await.unwrap();
    let bases: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event)
                if event.details.space_id() == Some(&upper.id)
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

#[tokio::test]
async fn setting_a_base_rejects_cycles_and_other_repositories_and_journals_changes() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let mut workspaces = Vec::new();
    for (repo, name) in [
        (&repos[0], "lower"),
        (&repos[0], "upper"),
        (&repos[1], "foreign"),
    ] {
        workspaces.push(
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap(),
        );
    }
    let [lower, upper, foreign] = &workspaces[..] else {
        unreachable!()
    };
    let set = |workspace: &str, base: Option<&str>| {
        let manager = manager.clone();
        let (workspace, base) = (workspace.to_owned(), base.map(str::to_owned));
        async move {
            manager
                .set_base_workspace(&SpaceSelector::parse(&workspace), base)
                .await
        }
    };

    manager
        .executions
        .issue_scope(
            "scope".into(),
            crate::daemon::scope::Caller {
                space_id: upper.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&upper.name, true), (&lower.name, false)] {
        let mut method = crate::protocol::WorkspaceMethod::SetBaseWorkspace {
            workspace: SpaceSelector::parse(target),
            base: Some(lower.name.to_string()),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed);
    }

    let stacked = set("upper", Some("lower")).await.unwrap();
    assert_eq!(stacked.base_workspace, Some(reference(lower)));
    for (workspace, base) in [
        ("upper", Some(lower.id.as_str())),
        ("lower", Some("upper")),
        ("lower", Some("lower")),
        ("upper", Some("foreign")),
        ("upper", Some("missing")),
    ] {
        assert!(
            set(workspace, base).await.is_err(),
            "{workspace} on {base:?}"
        );
    }
    assert_eq!(set("upper", None).await.unwrap().base_workspace, None);
    assert!(set(&foreign.name, None).await.is_err());

    let events = manager.workspace_events(None, 100).await.unwrap();
    let changes: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::BaseChanged => Some((
                event.details.space_id()?.clone(),
                event.details.cause,
                event.details.base_workspace.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        [
            (
                upper.id.clone(),
                Some(EventCause::Manual),
                Some(Some(reference(lower)))
            ),
            (upper.id.clone(), Some(EventCause::Manual), Some(None)),
        ]
    );
    // A base is also named by its space's number.
    let number = lower.number.to_string();
    let stacked = set("upper", Some(&number)).await.unwrap();
    assert_eq!(stacked.base_workspace, Some(reference(lower)));
}
