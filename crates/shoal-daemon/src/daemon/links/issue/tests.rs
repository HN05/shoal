use super::*;
use crate::daemon::manager::Manager;
use crate::daemon::{
    events::{EventItem, EventKind},
    test_support::{git, manager, repository},
};
use shoal_protocol::{links::issue::Registration, space_names::SpaceSelector};

#[tokio::test]
async fn association_is_persistent_idempotent_and_bound_to_the_origin() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    git(&path, &["config", "protocol.allow", "never"]);
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    manager
        .links
        .set_issue(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &url.parse().unwrap(),
            None,
            Assigner::User,
        )
        .await
        .unwrap();
    manager
        .links
        .set_issue(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &"316".parse().unwrap(),
            Some("Show titles".into()),
            Assigner::User,
        )
        .await
        .unwrap();
    manager
        .links
        .set_issue(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &"316".parse().unwrap(),
            None,
            Assigner::User,
        )
        .await
        .unwrap();
    let links = manager
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap()
        .links;
    let issue = links.issue().unwrap();
    assert_eq!(issue.url.url(), url);
    assert_eq!(issue.title.as_deref(), Some("Show titles"));
    let linked: Vec<_> = manager
        .space_events(None, 100)
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
    assert_eq!(linked[0].kind, crate::forge::items::link::LinkKind::Issue);
    assert_eq!(linked[0].url, url);
    // A second issue joins the first.
    manager
        .links
        .set_issue(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &"317".parse().unwrap(),
            None,
            Assigner::User,
        )
        .await
        .unwrap();
    assert!(
        manager
            .links
            .set_issue(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                &"https://github.com/other/repo/issues/316".parse().unwrap(),
                None,
                Assigner::User,
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .spaces
            .cleanup_snapshot(&*manager, &workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let inspected = reopened
        .spaces
        .inspect_workspace(&*reopened, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let numbers = |issues: &[Registration]| -> Vec<u64> {
        issues
            .iter()
            .map(|issue| issue.url.number().get())
            .collect()
    };
    assert_eq!(numbers(&inspected.issues), [316, 317]);
    assert!(
        reopened
            .links
            .clear_issue(
                &*reopened,
                &SpaceSelector::id(&workspace.id),
                Some(&"318".parse().unwrap())
            )
            .await
            .is_err()
    );
    // Unlinking one issue keeps the others.
    reopened
        .links
        .clear_issue(
            &*reopened,
            &SpaceSelector::id(&workspace.id),
            Some(&"316".parse().unwrap()),
        )
        .await
        .unwrap();
    assert!(reopened.space_events(None, 100).await.unwrap().iter().any(
        |item| matches!(item, EventItem::Event(event) if event.details.kind == EventKind::Unlinked
                && event.details.link.as_ref().is_some_and(|link| link.url == url))
    ));
    let registrations = reopened
        .links
        .issue_registrations(&workspace.id)
        .await
        .unwrap();
    assert_eq!(numbers(&registrations), [317]);
    let links = reopened
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap()
        .links;
    assert!(links.issues[0].title.is_none());
    // An issue linked again comes after those still assigned.
    reopened
        .links
        .set_issue(
            &*reopened,
            &SpaceSelector::id(&workspace.id),
            &"316".parse().unwrap(),
            None,
            Assigner::User,
        )
        .await
        .unwrap();
    let registrations = reopened
        .links
        .issue_registrations(&workspace.id)
        .await
        .unwrap();
    assert_eq!(numbers(&registrations), [317, 316]);
    reopened
        .links
        .clear_issue(&*reopened, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert!(
        (reopened
            .links
            .issue_registrations(&workspace.id)
            .await
            .unwrap())
        .is_empty()
    );
    reopened.store.shutdown().await;
}
