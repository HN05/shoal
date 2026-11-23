use shoal_links::actions::head_repository;
use shoal_protocol::{model::Assigner, space_names::SpaceSelector};

use crate::forge::items::{
    action::Action,
    item::{Item, ItemState},
};
use crate::forge::{
    ForgeRepo,
    items::{create::PullOptions, link::ItemKind},
};
use shoal_links::place::Place;

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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        .links
        .keep_created(
            &*manager,
            &workspace.id,
            ItemKind::Issue,
            &item(1),
            true,
            Assigner::User,
        )
        .await
        .unwrap_err();
    let links = manager
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap()
        .links;
    assert_eq!(links.opened.len(), 1);

    manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    // Refused before the forge is consulted: the repository has no origin.
    let issue = NewIssue {
        title: "Follow-up".into(),
        ..NewIssue::default()
    };
    // Naming its own repository as the one it acts in changes nothing.
    let contexts = [
        ItemContext::space(SpaceSelector::id(&workspace.id)),
        ItemContext::Space {
            space: SpaceSelector::id(&workspace.id),
            repository: Some(workspace.repository().clone()),
        },
    ];
    for context in contexts {
        let error = manager
            .links
            .open_issue(&*manager, &context.into(), issue.clone(), false, None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot accept an issue"),
            "{error:#}"
        );
    }
    let error = manager
        .links
        .open_pull(
            &*manager,
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
        .links
        .keep_created(
            &*manager,
            &workspace.id,
            ItemKind::Issue,
            &item(2),
            true,
            Assigner::User,
        )
        .await
        .unwrap();
    assert!(!linked);
    assert!(!warning.is_empty());
    let links = manager
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap()
        .links;
    assert_eq!(links.opened.len(), 1);
    let created = manager
        .space_events(None, 100)
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
async fn a_merge_naming_no_method_takes_the_configured_one() {
    use crate::{
        daemon::test_support::{commit, configure, manager, repository},
        forge::items::action::MergeMethod,
    };
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    std::fs::write(path.join(".shoal.toml"), "[merge]\nmethod = \"rebase\"\n").unwrap();
    commit(&path, ".shoal.toml");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    configure(&manager, "[merge]\nmethod = \"squash\"\n").await;
    let merge = |method| Action::Merge {
        method,
        message: None,
        delete_branch: false,
        head: None,
    };
    let method = async |place: Option<&Place>, mut action| {
        shoal_links::actions::configure_merge(&*manager, place, &mut action)
            .await
            .unwrap();
        match action {
            Action::Merge { method, .. } => method,
            other => panic!("not a merge: {other:?}"),
        }
    };
    let checkout = Place::Checkout(repo);
    assert_eq!(
        method(Some(&checkout), merge(None)).await,
        Some(MergeMethod::Rebase)
    );
    // Another repository than the request's own takes the global config's.
    assert_eq!(method(None, merge(None)).await, Some(MergeMethod::Squash));
    assert_eq!(
        method(Some(&checkout), merge(Some(MergeMethod::Merge))).await,
        Some(MergeMethod::Merge)
    );
}
