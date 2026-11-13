use super::*;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    client::events::{EventItem, LinkEvent},
    daemon::test_support::{git, manager, repository},
    model::LinkedItem,
};

async fn workspace_with_origin() -> (tempfile::TempDir, std::sync::Arc<Manager>, SpaceId) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    git(&path, &["config", "protocol.allow", "never"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(
            &repo.id,
            "relations".into(),
            Some("HEAD".into()),
            None,
            None,
        )
        .await
        .unwrap();
    git(
        &path,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    (root, manager, workspace.id)
}

async fn link_events(manager: &Manager, kind: EventKind) -> Vec<LinkEvent> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == kind => {
                event.details.link.map(|link| *link)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn linking_an_item_again_replaces_its_recorded_relation() {
    let (_root, manager, id) = workspace_with_origin().await;
    let issue = "https://github.com/team/repo/issues/40";
    let pr = "https://github.com/team/repo/pull/41";
    manager
        .link_item(
            &SpaceSelector::id(&id),
            LinkKind::Issue,
            &"40".parse().unwrap(),
            Relation::Opened,
            Some("Report".into()),
            None,
        )
        .await
        .unwrap();
    manager
        .link_item(
            &SpaceSelector::id(&id),
            LinkKind::Issue,
            &issue.parse().unwrap(),
            Relation::Opened,
            Some("Follow-up".into()),
            None,
        )
        .await
        .unwrap();
    manager
        .link_item(
            &SpaceSelector::id(&id),
            LinkKind::Pr,
            &"41".parse().unwrap(),
            Relation::Related,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .link_item(
            &SpaceSelector::id(&id),
            LinkKind::Issue,
            &"40".parse().unwrap(),
            Relation::Related,
            Some("Report".into()),
            None,
        )
        .await
        .unwrap();

    let links = manager.workspace_by_id(&id).await.unwrap().links;
    assert!(links.issues.is_empty());
    assert!(links.prs.is_empty());
    assert!(links.opened.is_empty());
    assert_eq!(
        links.related[1],
        LinkedItem {
            url: issue.parse().unwrap(),
            title: Some("Report".into()),
        }
    );
    let related: Vec<_> = links.related.iter().map(|item| item.url.url()).collect();
    assert_eq!(related, [pr, issue]);
    let linked = link_events(&manager, EventKind::Linked).await;
    assert_eq!(linked.len(), 3);
    assert_eq!(linked[0].relation, Relation::Opened);
    assert_eq!(linked[1].relation, Relation::Related);
    assert!(
        manager
            .link_item(
                &SpaceSelector::id(&id),
                LinkKind::Issue,
                &"https://github.com/other/repo/issues/1".parse().unwrap(),
                Relation::Related,
                Some("Report".into()),
                None,
            )
            .await
            .is_err()
    );

    manager
        .unlink_items(
            &SpaceSelector::id(&id),
            Relation::Related,
            Some(LinkKind::Pr),
            Some(&"41".parse().unwrap()),
        )
        .await
        .unwrap();
    assert!(
        manager
            .unlink_items(
                &SpaceSelector::id(&id),
                Relation::Related,
                Some(LinkKind::Pr),
                Some(&"41".parse().unwrap())
            )
            .await
            .is_err()
    );
    manager
        .unlink_items(&SpaceSelector::id(&id), Relation::Related, None, None)
        .await
        .unwrap();
    assert!(
        manager
            .unlink_items(&SpaceSelector::id(&id), Relation::Related, None, None)
            .await
            .is_err()
    );
    let links = manager.workspace_by_id(&id).await.unwrap().links;
    assert!(links.related.is_empty() && links.opened.is_empty());
    let unlinked = link_events(&manager, EventKind::Unlinked).await;
    let relations: Vec<_> = unlinked.iter().map(|link| link.relation).collect();
    assert_eq!(
        relations,
        [Relation::Opened, Relation::Related, Relation::Related]
    );
}

async fn link_issue(manager: &Manager, id: &SpaceId, relation: Relation) -> Result<Linked> {
    let input = "7".parse().unwrap();
    let title = Some("Fix".into());
    (manager.link_item(
        &SpaceSelector::id(id),
        LinkKind::Issue,
        &input,
        relation,
        title,
        None,
    ))
    .await
}

#[tokio::test]
async fn an_assignment_replaces_the_recorded_relation_and_refuses_another() {
    let (_root, manager, id) = workspace_with_origin().await;
    link_issue(&manager, &id, Relation::Related).await.unwrap();
    link_issue(&manager, &id, Relation::Assigned).await.unwrap();

    let links = manager.workspace_by_id(&id).await.unwrap().links;
    let issue = links.issue().map(|issue| issue.url.url());
    assert_eq!(issue, Some("https://github.com/team/repo/issues/7"));
    assert!(links.related.is_empty());
    for relation in [Relation::Opened, Relation::Related] {
        let refused = link_issue(&manager, &id, relation).await.unwrap_err();
        assert!(format!("{refused:#}").contains("linked as assigned"));
    }
    let links = manager.workspace_by_id(&id).await.unwrap().links;
    assert!(links.issue().is_some() && links.opened.is_empty() && links.related.is_empty());
}

#[tokio::test]
async fn assigned_relation_uses_the_issue_link() {
    let (_root, manager, id) = workspace_with_origin().await;
    let issue = "https://github.com/team/repo/issues/7";
    manager
        .link_item(
            &SpaceSelector::id(&id),
            LinkKind::Issue,
            &"7".parse().unwrap(),
            Relation::Assigned,
            Some("Fix".into()),
            None,
        )
        .await
        .unwrap();
    let links = manager.workspace_by_id(&id).await.unwrap().links;
    assert_eq!(links.issue().map(|linked| linked.url.url()), Some(issue));
    assert!(links.opened.is_empty() && links.related.is_empty());
    assert_eq!(
        link_events(&manager, EventKind::Linked).await[0].relation,
        Relation::Assigned
    );
    manager
        .unlink_items(
            &SpaceSelector::id(&id),
            Relation::Assigned,
            Some(LinkKind::Issue),
            None,
        )
        .await
        .unwrap();
    assert!((manager.workspace_by_id(&id).await.unwrap().links.issues).is_empty());
    for kind in [Some(LinkKind::Issue), None] {
        let unlinked = manager
            .unlink_items(&SpaceSelector::id(&id), Relation::Assigned, kind, None)
            .await;
        assert!(unlinked.is_err());
    }
}

#[tokio::test]
async fn a_moved_worktree_takes_no_recorded_relations() {
    let (_root, manager, id) = workspace_with_origin().await;
    let path = manager.workspace_by_id(&id).await.unwrap().path;
    std::fs::remove_dir_all(&path).unwrap();
    assert!(
        manager
            .link_item(
                &SpaceSelector::id(&id),
                LinkKind::Issue,
                &"40".parse().unwrap(),
                Relation::Related,
                Some("Report".into()),
                None
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .unlink_items(&SpaceSelector::id(&id), Relation::Related, None, None)
            .await
            .is_err()
    );
}

#[test]
fn link_events_from_before_relations_were_assignments() {
    let link: LinkEvent =
        serde_json::from_str(r#"{"kind":"pr","url":"https://forge.example/team/repo/pulls/7"}"#)
            .unwrap();
    assert_eq!(link.relation, Relation::Assigned);
}

#[tokio::test]
async fn an_issue_linked_without_its_title_gets_the_forges() {
    let server = crate::forge::test_support::Server::start(|request| {
        let number = request
            .line
            .strip_prefix("GET /api/v1/repos/team/repo/issues/");
        let Some(number) = number.filter(|number| *number != "9") else {
            return (404, Vec::new(), Vec::new());
        };
        let issue = serde_json::json!({"number": number.parse::<u64>().unwrap(),
            "title": format!("Issue {number}"), "state": "open"});
        (200, Vec::new(), issue.to_string().into())
    });
    let host = &server.host;
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    let (root, manager) = manager().await;
    crate::daemon::test_support::configure(
        &manager,
        &format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n"),
    )
    .await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "titles".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{host}/team/repo"),
        ],
    );
    let selector = SpaceSelector::id(&workspace.id);
    let link = |input: &str, relation| {
        let input = input.parse().unwrap();
        let (manager, selector) = (&manager, &selector);
        async move { (manager.link_item(selector, LinkKind::Issue, &input, relation, None, None)).await }
    };
    for (input, relation) in [("5", Relation::Assigned), ("6", Relation::Related)] {
        assert_eq!(link(input, relation).await.unwrap().title_error, None);
    }
    // An issue the forge does not show is linked without a title.
    let unread = link("9", Relation::Opened).await.unwrap();

    let links = manager.workspace_by_id(&workspace.id).await.unwrap().links;
    assert_eq!(links.issues[0].title.as_deref(), Some("Issue 5"));
    assert_eq!(links.related[0].title.as_deref(), Some("Issue 6"));
    assert!(unread.title_error.is_some());
    assert_eq!(links.opened[0].title, None);
}
