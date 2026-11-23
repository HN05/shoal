use crate::daemon::scope::Access;
use crate::model::EventCause;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        test_support::{manager, repository},
    },
    model::WorkspaceRef,
};
use shoal_protocol::space_names::SpaceSelector;

fn reference(workspace: &crate::model::Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch().cloned(),
    }
}

#[tokio::test]
async fn setting_a_base_rejects_cycles_and_other_repositories_and_journals_changes() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .repositories
                .register(checkout.to_str().unwrap().into(), None, None)
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
    let set = |workspace: &crate::model::Workspace, base: Option<&str>| {
        let manager = manager.clone();
        let (workspace, base) = (SpaceSelector::id(&workspace.id), base.map(str::to_owned));
        async move { manager.spaces.set_base_workspace(&workspace, base).await }
    };

    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            crate::daemon::scope::Caller {
                space_id: upper.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    for (target, allowed) in [(&upper.id, true), (&lower.id, false)] {
        let mut method = crate::protocol::WorkspaceMethod::SetBaseWorkspace {
            workspace: SpaceSelector::from(target),
            base: Some(lower.name.to_string()),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed);
    }

    let stacked = set(upper, Some("lower")).await.unwrap();
    assert_eq!(stacked.base_workspace, Some(reference(lower)));
    for (workspace, base) in [
        (upper, Some(lower.id.as_str())),
        (lower, Some("upper")),
        (lower, Some("lower")),
        (upper, Some("foreign")),
        (upper, Some("missing")),
    ] {
        assert!(
            set(workspace, base).await.is_err(),
            "{} on {base:?}",
            workspace.name
        );
    }
    assert_eq!(set(upper, None).await.unwrap().base_workspace, None);
    assert!(set(foreign, None).await.is_err());

    let events = manager.space_events(None, 100).await.unwrap();
    let changes: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::BaseChanged => Some((
                event.details.space_id.clone(),
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
    let stacked = set(upper, Some(&number)).await.unwrap();
    assert_eq!(stacked.base_workspace, Some(reference(lower)));
}
