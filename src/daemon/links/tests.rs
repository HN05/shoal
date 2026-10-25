use super::*;
use crate::{
    daemon::events::{EventItem, LinkEvent},
    model::LinkedItem,
    test_support::{git, manager, repository},
};

async fn workspace_with_origin() -> (tempfile::TempDir, std::sync::Arc<Manager>, String) {
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
async fn recorded_relations_are_kept_apart_from_the_assignment() {
    let (_root, manager, id) = workspace_with_origin().await;
    let issue = "https://github.com/team/repo/issues/40";
    let pr = "https://github.com/team/repo/pull/41";
    manager
        .link_item(&id, ItemKind::Issue, "40", Relation::Opened, None)
        .await
        .unwrap();
    manager
        .link_item(
            &id,
            ItemKind::Issue,
            issue,
            Relation::Opened,
            Some("Follow-up".into()),
        )
        .await
        .unwrap();
    manager
        .link_item(&id, ItemKind::Pr, "41", Relation::Related, None)
        .await
        .unwrap();
    manager
        .link_item(&id, ItemKind::Issue, "40", Relation::Related, None)
        .await
        .unwrap();

    let links = manager.workspace(&id).await.unwrap().links;
    assert_eq!(links.issue, None);
    assert!(links.prs.is_empty());
    assert_eq!(
        links.opened,
        [LinkedItem {
            kind: ItemKind::Issue,
            url: issue.into(),
            title: Some("Follow-up".into()),
        }]
    );
    let related: Vec<_> = links.related.iter().map(|item| item.url.as_str()).collect();
    assert_eq!(related, [pr, issue]);
    let linked = link_events(&manager, EventKind::Linked).await;
    assert_eq!(linked.len(), 3);
    assert_eq!(linked[0].relation, Relation::Opened);
    assert_eq!(linked[1].relation, Relation::Related);
    assert!(
        manager
            .link_item(
                &id,
                ItemKind::Issue,
                "https://github.com/other/repo/issues/1",
                Relation::Related,
                None
            )
            .await
            .is_err()
    );

    manager
        .unlink_items(&id, Relation::Related, Some(ItemKind::Pr), Some("41"))
        .await
        .unwrap();
    assert!(
        manager
            .unlink_items(&id, Relation::Related, Some(ItemKind::Pr), Some("41"))
            .await
            .is_err()
    );
    manager
        .unlink_items(&id, Relation::Related, None, None)
        .await
        .unwrap();
    let error = manager
        .unlink_items(&id, Relation::Related, None, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("nothing is linked as related"),
        "{error:#}"
    );
    let links = manager.workspace(&id).await.unwrap().links;
    assert!(links.related.is_empty());
    assert_eq!(links.opened.len(), 1);
    let unlinked = link_events(&manager, EventKind::Unlinked).await;
    assert_eq!(unlinked.len(), 2);
    assert!(
        unlinked
            .iter()
            .all(|link| link.relation == Relation::Related)
    );
}

#[tokio::test]
async fn assigned_relation_uses_the_issue_link() {
    let (_root, manager, id) = workspace_with_origin().await;
    let issue = "https://github.com/team/repo/issues/7";
    manager
        .link_item(
            &id,
            ItemKind::Issue,
            "7",
            Relation::Assigned,
            Some("Fix".into()),
        )
        .await
        .unwrap();
    let links = manager.workspace(&id).await.unwrap().links;
    assert_eq!(links.issue.as_deref(), Some(issue));
    assert!(links.opened.is_empty() && links.related.is_empty());
    assert_eq!(
        link_events(&manager, EventKind::Linked).await[0].relation,
        Relation::Assigned
    );
    manager
        .unlink_items(&id, Relation::Assigned, Some(ItemKind::Issue), None)
        .await
        .unwrap();
    assert_eq!(manager.workspace(&id).await.unwrap().links.issue, None);
    for kind in [Some(ItemKind::Issue), None] {
        let error = manager
            .unlink_items(&id, Relation::Assigned, kind, None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("linked as assigned"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn a_moved_worktree_takes_no_recorded_relations() {
    let (_root, manager, id) = workspace_with_origin().await;
    let path = manager.workspace(&id).await.unwrap().path;
    std::fs::remove_dir_all(&path).unwrap();
    assert!(
        manager
            .link_item(&id, ItemKind::Issue, "40", Relation::Related, None)
            .await
            .is_err()
    );
    assert!(
        manager
            .unlink_items(&id, Relation::Related, None, None)
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
