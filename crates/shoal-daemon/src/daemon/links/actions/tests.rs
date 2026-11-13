use super::*;
use crate::client::space_names::SpaceSelector;

#[tokio::test]
async fn a_branch_pushed_to_a_fork_on_the_same_forge_names_the_fork_as_its_head() {
    use crate::daemon::test_support::{git, repository};
    let root = tempfile::tempdir().unwrap();
    let path = repository(root.path(), "repo");
    let forge = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let topic = "topic".into();
    let head = || head_repository(&path, &topic, &forge);
    git(
        &path,
        &["remote", "add", "origin", "git@github.com:team/repo.git"],
    );
    assert_eq!(head().await.unwrap(), None);
    git(
        &path,
        &["remote", "add", "fork", "git@github.com:someone/repo.git"],
    );
    git(&path, &["config", "remote.pushDefault", "fork"]);
    assert_eq!(head().await.unwrap().as_deref(), Some("someone/repo"));
    git(
        &path,
        &["remote", "set-url", "--push", "fork", "/elsewhere/repo.git"],
    );
    // A push URL elsewhere does not change which repository the remote is.
    assert_eq!(head().await.unwrap().as_deref(), Some("someone/repo"));
    git(
        &path,
        &[
            "remote",
            "set-url",
            "fork",
            "https://gitlab.com/someone/repo",
        ],
    );
    assert!(head().await.is_err());
}

#[tokio::test]
async fn a_workspace_being_removed_opens_nothing_and_keeps_late_items_unlinked() {
    use crate::{
        daemon::test_support::{manager, repository},
        forge::items::create::NewIssue,
        protocol::ItemContext,
        state::WorkspaceState,
    };
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let item = |number: u64| Item {
        kind: ItemKind::Issue,
        number,
        url: format!("https://forge.example/team/repo/issues/{number}")
            .parse()
            .unwrap(),
        title: "Follow-up".into(),
        state: ItemState::Open,
        labels: Vec::new(),
        assignees: Vec::new(),
        milestone: None,
        pr: None,
    };

    // A link failing for another reason still fails, after the record.
    manager
        .keep_created(&workspace.id, ItemKind::Issue, &item(1), true)
        .await
        .unwrap_err();
    let links = manager.workspace_by_id(&workspace.id).await.unwrap().links;
    assert_eq!(links.opened.len(), 1);

    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    // Refused before the forge is consulted: the repository has no origin.
    let issue = NewIssue {
        title: "Follow-up".into(),
        ..NewIssue::default()
    };
    let context = ItemContext::Workspace(SpaceSelector::id(&workspace.id));
    let error = manager
        .open_issue(&context, issue, None, false, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot accept an issue"),
        "{error:#}"
    );
    let error = manager
        .open_pull(
            &SpaceSelector::id(&workspace.id),
            PullOptions::default(),
            false,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot accept a PR"),
        "{error:#}"
    );

    // An item created after the check but before removal reserved the
    // workspace keeps its creation event and comes back with a warning.
    let (linked, warning) = manager
        .keep_created(&workspace.id, ItemKind::Issue, &item(2), true)
        .await
        .unwrap();
    assert!(!linked);
    assert!(warning.is_some());
    let links = manager.workspace_by_id(&workspace.id).await.unwrap().links;
    assert_eq!(links.opened.len(), 1);
    let created = manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| {
            matches!(item,
                crate::daemon::events::EventItem::Event(event) if event.details.item.is_some())
        })
        .count();
    assert_eq!(created, 2);
}

#[tokio::test]
async fn a_linked_item_of_an_earlier_origin_is_not_taken_for_the_current_ones() {
    use crate::daemon::test_support::{manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                rusqlite::params![id, "https://forge.example/team/old/issues/7"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let linked = |remote: &str| {
        let forge = ForgeRepo::parse(remote).unwrap();
        let manager = &manager;
        let id = workspace.id.clone();
        async move {
            (manager.named_or_linked(None, Some(&id), &forge, ItemKind::Issue))
                .await
                .map(|issue| issue.number().get())
        }
    };
    assert_eq!(linked("https://forge.example/team/old").await.unwrap(), 7);
    // After its origin moved to another repository, issue 7 there is another issue.
    assert!(linked("https://forge.example/team/new").await.is_err());
}

#[tokio::test]
async fn several_assigned_issues_need_the_number_of_the_one_acted_on() {
    use crate::daemon::test_support::{manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            for number in [7, 8] {
                db.execute(
                    "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                    rusqlite::params![
                        id,
                        format!("https://forge.example/team/repo/issues/{number}")
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let linked = |named: Option<&str>| {
        let named = named.map(|named| named.parse().unwrap());
        let (manager, forge, id) = (&manager, &forge, workspace.id.clone());
        async move {
            (manager.named_or_linked(named, Some(&id), forge, ItemKind::Issue))
                .await
                .map(|issue| issue.number().get())
        }
    };
    let error = linked(None).await.unwrap_err();
    assert!(format!("{error:#}").contains("several"), "{error:#}");
    assert_eq!(linked(Some("8")).await.unwrap(), 8);
}
