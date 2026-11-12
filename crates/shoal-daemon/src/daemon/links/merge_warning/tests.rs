use std::fs;

use rusqlite::params;

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    links::pr::{Registration, RegistrationKind},
    test_support::{commit, manager, repository},
};

/// A workspace whose `version`, listed as sensitive, the default branch
/// changed too, checked once.
async fn changed_on_both_sides() -> (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    crate::model::Workspace,
) {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    fs::write(
        repo.join(".shoal.toml"),
        "[conflicts]\nsensitive = ['version']\n",
    )
    .unwrap();
    commit(&repo, ".shoal.toml");
    let repo_id = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
        .id;
    let workspace = manager
        .create_workspace(&repo_id, "topic".into(), None, None, None)
        .await
        .unwrap();
    // Unlike the workspace's, the default branch's change follows another.
    fs::write(repo.join("other"), "main\n").unwrap();
    commit(&repo, "other");
    for dir in [&workspace.path, &repo] {
        fs::write(dir.join("version"), "2\n").unwrap();
        commit(dir, "version");
    }
    fs::write(workspace.path.join("feature"), "new\n").unwrap();
    commit(&workspace.path, "feature");
    manager
        .check_conflicts(&SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn a_branch_merge_warns_only_at_the_checked_head() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let warning = manager
        .branch_merge_warning(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (warning.subject.as_str(), warning.target.as_str()),
        ("topic", "main")
    );
    assert_eq!(warning.files, ["version"]);

    let current = manager.current_changed_on_both_sides(&workspace).await;
    assert!(current.unwrap().is_some());

    fs::write(workspace.path.join("feature"), "newer\n").unwrap();
    commit(&workspace.path, "feature");
    assert!(
        manager
            .branch_merge_warning(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_none()
    );
    let current = manager.current_changed_on_both_sides(&workspace).await;
    assert!(
        current.unwrap().is_none(),
        "status shows no check of an earlier head"
    );
}

#[tokio::test]
async fn a_branch_merge_ignores_a_check_against_a_base_workspaces_branch() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let repo_id = workspace.repository_id.clone();
    let base = manager
        .create_workspace(&repo_id, "base".into(), None, None, None)
        .await
        .unwrap();
    manager
        .set_base_workspace(
            &SpaceSelector::id(&workspace.id),
            Some(base.name.to_string()),
        )
        .await
        .unwrap();
    fs::write(base.path.join("version"), "3\n").unwrap();
    commit(&base.path, "version");
    let check = manager
        .check_conflicts(&SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert_eq!(Some(check.target), base.branch);
    assert_eq!(check.changed_on_both_sides, ["version"]);
    assert!(
        manager
            .branch_merge_warning(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_pr_finds_the_record_of_the_workspace_that_links_it() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let url = "https://forge.example/team/project/pulls/7".to_owned();
    assert!(
        manager
            .recorded_for_pull(url.clone())
            .await
            .unwrap()
            .is_empty()
    );

    let registration = Registration {
        kind: RegistrationKind::Watch {
            urls: vec![url.clone()],
            merged_head: None,
        },
        error: None,
    };
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let recorded = manager.recorded_for_pull(url).await.unwrap();
    let head = recorded[0].head.clone();
    let into = |target: &str| Merged {
        head: head.clone().into(),
        target: target.into(),
    };
    let found = warning("PR #7".into(), recorded.clone(), &into("main")).unwrap();
    assert_eq!(
        (found.target.as_str(), found.files.as_slice()),
        ("main", &["version".to_owned()][..])
    );
    // A PR into another branch merges nothing the check of main found.
    assert!(warning("PR #7".into(), recorded, &into("release")).is_none());
}

#[tokio::test]
async fn a_merge_is_refused_once_its_warning_is_not_the_one_confirmed() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let checkout = manager.repository(&workspace.repository_id).await.unwrap();
    crate::daemon::test_support::git(
        &checkout.path,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let context = ItemContext::Workspace(SpaceSelector::id(&workspace.id));
    // Nothing links PR 7, so merging it warns about nothing.
    let confirmed = ConfirmedWarning {
        warning: Some(MergeWarning {
            subject: "PR #7".into(),
            target: "main".into(),
            files: vec!["version".into()],
            url: Some("https://github.com/team/repo/pull/7".into()),
        }),
    };
    let merge = Action::Merge {
        method: crate::forge::items::action::MergeMethod::Merge,
        delete_branch: false,
    };
    let refused = manager
        .confirmed_merge(&context, Some("7".into()), merge, &confirmed)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("changed since you confirmed"));
    let refused = manager
        .confirmed_merge(&context, Some("7".into()), Action::Close, &confirmed)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("only a merge"));
}

/// Watch the PR at `url` for the workspace `id`, its one linked PR, in the
/// database at `database`.
fn link_pull(database: &std::path::Path, id: &str, url: &str) {
    let registration = Registration {
        kind: RegistrationKind::Watch {
            urls: vec![url.to_owned()],
            merged_head: None,
        },
        error: None,
    };
    let db = rusqlite::Connection::open(database).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(60)).unwrap();
    db.execute(
        "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET record=excluded.record",
        params![id, serde_json::to_string(&registration).unwrap()],
    )
    .unwrap();
}

#[tokio::test]
async fn a_confirmed_merge_keeps_the_pr_whose_warning_it_read() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let head = crate::daemon::links::pr::current_head(&workspace)
        .await
        .unwrap();
    let database = manager.paths.database();
    let id = workspace.id.to_string();
    let host = std::sync::Arc::new(std::sync::OnceLock::<String>::new());
    let served = host.clone();
    let reads = std::sync::atomic::AtomicUsize::new(0);
    // Answering the merge's read of PR 1's warning, the second after the
    // user's, links PR 2 instead.
    let server = crate::forge::test_support::Server::start(move |request| {
        let read = request.line == "GET /api/v1/repos/team/repo/pulls/1";
        if read && reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            let other = format!("http://{}/team/repo/pulls/2", served.get().unwrap());
            link_pull(&database, &id, &other);
        }
        let pull = serde_json::json!({
            "number": 1, "title": "Fix", "state": "open", "merged": false,
            "draft": false, "head": {"ref": "topic", "sha": head},
            "base": {"ref": "main"}, "labels": [],
        });
        (200, Vec::new(), pull.to_string().into())
    });
    host.set(server.host.clone()).unwrap();
    let host = &server.host;
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    crate::daemon::test_support::configure(
        &manager,
        &format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n"),
    )
    .await;
    let checkout = manager.repository(&workspace.repository_id).await.unwrap();
    let origin = format!("http://{host}/team/repo");
    crate::daemon::test_support::git(&checkout.path, &["remote", "add", "origin", &origin]);
    let first = format!("http://{host}/team/repo/pulls/1");
    link_pull(&manager.paths.database(), workspace.id.as_str(), &first);

    let context = ItemContext::Workspace(SpaceSelector::id(&workspace.id));
    let warning = manager.merge_warning(&context, None).await.unwrap();
    assert!(warning.is_some());
    let merge = Action::Merge {
        method: crate::forge::items::action::MergeMethod::Merge,
        delete_branch: false,
    };
    let confirmed = ConfirmedWarning { warning };
    // Whatever the forge answers after the merge, it merged PR 1.
    let _ = (manager.confirmed_merge(&context, None, merge, &confirmed)).await;
    let merges: Vec<_> = (server.received().into_iter())
        .filter(|request| request.line.starts_with("POST"))
        .map(|request| request.line)
        .collect();
    assert!(!merges.is_empty());
    assert!(
        merges.iter().all(|line| line.contains("/pulls/1/")),
        "{merges:?}"
    );
}
