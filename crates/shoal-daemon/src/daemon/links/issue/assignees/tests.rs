use crate::daemon::links::issue::test_support::{Fixture, Refuse, USER};
use crate::daemon::scope::Access;
use crate::{
    forge::items::reference::ItemRef,
    model::{Assigner, IssueAssignee, Workspace},
    removal::{InspectionPolicy, UnsavedWork},
};
use serde_json::json;
use shoal_protocol::{alerts::AlertKind, space_names::SpaceSelector};

#[tokio::test]
async fn an_assignee_shoal_did_not_add_stays() {
    let fixture = Fixture::new(&["fix"], &["shoal-user"], USER).await;
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    fixture.unlink(workspace).await;
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn the_assignee_stays_while_another_space_of_the_login_links_the_issue() {
    let fixture = Fixture::new(&["first", "second"], &[], USER).await;
    let [first, second] = &fixture.workspaces[..] else {
        unreachable!()
    };
    fixture.link(first).await;
    fixture.link(second).await;
    fixture.unlink(first).await;
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
    // Removing the last space that links it takes it back.
    let selector = SpaceSelector::id(&second.id);
    (fixture.manager)
        .spaces
        .remove_selected(
            &*fixture.manager,
            &selector,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert_eq!(fixture.changes(), [json!(["shoal-user"]), json!([])]);
}

#[tokio::test]
async fn an_assignee_a_removal_cannot_take_back_notifies_the_user() {
    let fixture = Fixture::new(&["fix"], &[], USER).await;
    fixture.refuse(Refuse::Removing);
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.manager)
        .spaces
        .remove_selected(
            &*fixture.manager,
            &selector,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let notifications = fixture.manager.notifications.list(true, 10).await.unwrap();
    let kept = (notifications.iter())
        .find(|notification| notification.kind == AlertKind::UnassignFailed)
        .unwrap();
    assert!(
        kept.message.contains("--remove-assignee shoal-user"),
        "{}",
        kept.message
    );
}

#[tokio::test]
async fn a_moved_space_leaves_its_assignee_to_the_other_machine() {
    let fixture = Fixture::new(&["fix"], &[], USER).await;
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    let manager = &fixture.manager;
    manager
        .links
        .hand_over_assignees(&workspace.id)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.spaces.remove_selected(
        &**manager,
        &selector,
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
}

#[tokio::test]
async fn a_restored_space_takes_back_the_assignee_shoal_added_before() {
    let fixture = Fixture::new(&["fix"], &[], USER).await;
    let workspace = &fixture.workspaces[0];
    let (manager, selector) = (&fixture.manager, SpaceSelector::id(&workspace.id));
    fixture.link(workspace).await;
    // The removal could not take it back, so the forge still shows it.
    fixture.refuse(Refuse::Removing);
    (manager.spaces.remove_selected(
        &**manager,
        &selector,
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    fixture.refuse(Refuse::Nothing);
    let restored = manager.restore_workspace(&selector).await.unwrap();
    fixture.unlink(&restored.opened.workspace).await;
    assert_eq!(
        fixture.changes(),
        [json!(["shoal-user"]), json!([]), json!([])]
    );
}

/// The links of a space whose issue 7 has the user's login, which Shoal
/// added.
fn carried(fixture: &Fixture) -> crate::model::SpaceLinks {
    let url = format!("http://{}/team/repo/issues/7", fixture.server.host);
    let url: ItemRef = url.parse().unwrap();
    crate::model::SpaceLinks {
        issues: vec![crate::model::LinkedItem {
            url: url.clone(),
            title: None,
        }],
        assignees: vec![IssueAssignee {
            url,
            account: Assigner::User,
            added: true,
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_scoped_relink_takes_on_no_assignment() {
    let fixture = Fixture::new(&["fix"], &["shoal-user"], USER).await;
    let workspace = &fixture.workspaces[0];
    let links = carried(&fixture);
    let caller = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let warnings = (fixture.manager.links.relink_space(
        &*fixture.manager,
        &workspace.id,
        &links,
        Some(&caller),
    ))
    .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    // The login was there already and the claim was ignored, so nothing
    // takes it back.
    fixture.unlink(workspace).await;
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn a_scoped_relink_checks_its_related_items_as_links_do() {
    let fixture = Fixture::new(&["fix"], &[], USER).await;
    let workspace = &fixture.workspaces[0];
    let elsewhere = crate::model::LinkedItem {
        url: "https://github.com/other/repo/issues/9".parse().unwrap(),
        title: None,
    };
    let links = crate::model::SpaceLinks {
        related: vec![elsewhere],
        ..Default::default()
    };
    let caller = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let manager = &fixture.manager;
    let warnings = (manager
        .links
        .relink_space(&**manager, &workspace.id, &links, Some(&caller)))
    .await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let related = |workspace: Workspace| workspace.links.related.len();
    assert_eq!(
        related(manager.spaces.workspace_by_id(&workspace.id).await.unwrap()),
        0
    );
    // The user's relink, as a restore makes, records it as it was.
    assert!(
        (manager
            .links
            .relink_space(&**manager, &workspace.id, &links, None))
        .await
        .is_empty()
    );
    assert_eq!(
        related(manager.spaces.workspace_by_id(&workspace.id).await.unwrap()),
        1
    );
}

#[tokio::test]
async fn linking_an_issue_others_are_assigned_names_them() {
    let fixture = Fixture::new(&["fix"], &["ann", "bob"], USER).await;
    let workspace = &fixture.workspaces[0];
    let warnings = fixture.link(workspace).await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ann, bob"), "{warnings:?}");
    assert_eq!(fixture.changes(), [json!(["ann", "bob", "shoal-user"])]);
}
