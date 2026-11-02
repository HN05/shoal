use std::fs;

use rusqlite::params;

use super::*;
use crate::{
    daemon::links::pr::{Registration, RegistrationKind},
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
    manager.check_conflicts(&workspace.id, None).await.unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn landing_warns_only_at_the_checked_head() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let warning = manager.land_warning(&workspace.id).await.unwrap().unwrap();
    assert_eq!(warning.to_string(), "topic and main both changed version");

    let current = manager.current_changed_on_both_sides(&workspace).await;
    assert!(current.unwrap().is_some());

    fs::write(workspace.path.join("feature"), "newer\n").unwrap();
    commit(&workspace.path, "feature");
    assert!(manager.land_warning(&workspace.id).await.unwrap().is_none());
    let current = manager.current_changed_on_both_sides(&workspace).await;
    assert!(
        current.unwrap().is_none(),
        "status shows no check of an earlier head"
    );
}

#[tokio::test]
async fn landing_ignores_a_check_against_a_base_workspaces_branch() {
    let (_root, manager, workspace) = changed_on_both_sides().await;
    let repo_id = workspace.repository_id.clone();
    let base = manager
        .create_workspace(&repo_id, "base".into(), None, None, None)
        .await
        .unwrap();
    manager
        .set_base_workspace(&workspace.id, Some(base.name.clone()))
        .await
        .unwrap();
    fs::write(base.path.join("version"), "3\n").unwrap();
    commit(&base.path, "version");
    let check = manager.check_conflicts(&workspace.id, None).await.unwrap();
    assert_eq!(check.target, base.branch);
    assert_eq!(check.changed_on_both_sides, ["version"]);
    assert!(manager.land_warning(&workspace.id).await.unwrap().is_none());
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
        head: head.clone(),
        target: target.into(),
    };
    let found = warning("PR #7".into(), recorded.clone(), &into("main")).unwrap();
    assert_eq!(found.to_string(), "PR #7 and main both changed version");
    // A PR into another branch merges nothing the check of main found.
    assert!(warning("PR #7".into(), recorded, &into("release")).is_none());
}
