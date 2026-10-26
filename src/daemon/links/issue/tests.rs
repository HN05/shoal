use super::*;
use crate::{
    daemon::events::{EventItem, EventKind},
    test_support::{git, manager, repository},
};

#[tokio::test]
async fn association_is_persistent_idempotent_and_bound_to_the_origin() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    git(&path, &["config", "protocol.allow", "never"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "issue".into(), Some("HEAD".into()), None, None)
        .await
        .unwrap();
    git(
        &path,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let url = "https://github.com/team/repo/issues/316";
    manager.set_issue(&workspace.id, url, None).await.unwrap();
    manager
        .set_issue(&workspace.id, "316", Some("Show titles".into()))
        .await
        .unwrap();
    manager.set_issue(&workspace.id, "316", None).await.unwrap();
    let links = manager.workspace(&workspace.id).await.unwrap().links;
    assert_eq!(links.issue.as_deref(), Some(url));
    assert_eq!(links.issue_title.as_deref(), Some("Show titles"));
    let linked: Vec<_> = manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::Linked => {
                event.details.link
            }
            _ => None,
        })
        .collect();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].kind, crate::forge::items::link::ItemKind::Issue);
    assert_eq!(linked[0].url, url);
    assert!(manager.set_issue(&workspace.id, "317", None).await.is_err());
    assert!(
        manager
            .set_issue(
                &workspace.id,
                "https://github.com/other/repo/issues/316",
                None
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert_eq!(
        reopened
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .issue
            .unwrap()
            .url,
        url
    );
    assert!(
        reopened
            .clear_issue(&workspace.id, Some("317"))
            .await
            .is_err()
    );
    assert!(
        reopened
            .issue_registration(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    reopened
        .clear_issue(&workspace.id, Some("316"))
        .await
        .unwrap();
    assert!(reopened.workspace_events(None, 100).await.unwrap().iter().any(
            |item| matches!(item, EventItem::Event(event) if event.details.kind == EventKind::Unlinked
                && event.details.link.as_ref().is_some_and(|link| link.url == url))
        ));
    assert!(
        reopened
            .issue_registration(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened
        .set_issue(&workspace.id, "317", None)
        .await
        .unwrap();
    assert!(
        reopened
            .workspace(&workspace.id)
            .await
            .unwrap()
            .links
            .issue_title
            .is_none()
    );
    assert!(
        reopened
            .issue_registration(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .url
            .ends_with("/317")
    );
    reopened.store.shutdown().await;
}
