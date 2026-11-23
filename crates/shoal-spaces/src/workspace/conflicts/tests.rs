use crate::stand_in::Fixture;
use shoal_core::test_support::{commit, git, repository};
use shoal_refs::BranchName;
use std::{fs, path::Path};

use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn conflicts_compare_committed_head_with_the_default_or_named_branch() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = repository(root.path(), "repo");
    let repo_id = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
        .id;
    let workspace = manager
        .create_workspace(&repo_id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let check = manager
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert_eq!(check.target, "main");
    assert!(!check.conflicts && check.files.is_empty());

    fs::write(workspace.path.join("tracked"), "workspace\n").unwrap();
    commit(&workspace.path, "tracked");
    fs::write(repo.join("tracked"), "main\n").unwrap();
    commit(&repo, "tracked");
    fs::write(repo.join("other"), "main\n").unwrap();
    commit(&repo, "other");
    let check = manager
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert!(check.conflicts);
    assert_eq!(check.files, ["tracked"]);
    assert_eq!(
        check.target_commit,
        git(&repo, &["rev-parse", "main"]).trim()
    );
    assert_eq!(
        check.head,
        git(&workspace.path, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(
        git(&workspace.path, &["status", "--porcelain"]),
        "",
        "the check leaves the worktree and index untouched"
    );

    git(&repo, &["branch", "release", "main~2"]);
    let check = manager
        .spaces
        .check_conflicts(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some("release".into()),
        )
        .await
        .unwrap();
    assert_eq!(check.target, "release");
    assert!(!check.conflicts);
    manager
        .spaces
        .check_conflicts(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some("missing".into()),
        )
        .await
        .unwrap_err();
    let lone = git(&repo, &["commit-tree", "main^{tree}", "-m", "lone"]);
    git(&repo, &["branch", "lone", lone.trim()]);
    let selected = SpaceSelector::id(&workspace.id);
    let unrelated = manager
        .spaces
        .check_conflicts(&*manager, &selected, Some("lone".into()));
    assert!(unrelated.await.is_err());

    let base = manager
        .create_workspace(&repo_id, "base".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .set_base_workspace(
            &SpaceSelector::id(&workspace.id),
            Some(base.name.to_string()),
        )
        .await
        .unwrap();
    let check = manager
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert_eq!(
        Some(check.target.as_str()),
        base.branch().map(BranchName::as_str)
    );
    assert_eq!(
        check.target_commit,
        git(&base.path, &["rev-parse", "HEAD"]).trim()
    );
}

#[tokio::test]
async fn sensitive_paths_changed_on_both_sides_are_listed_until_the_target_has_heads_changes() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = repository(root.path(), "repo");
    let listed = "[conflicts]\nsensitive = ['version', 'migrations/**', 'mine']\n";
    fs::write(repo.join(".shoal.toml"), listed).unwrap();
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
    // Identical changes merge cleanly, so only the listed paths stand out.
    let change = |dir: &Path, files: &[&str]| {
        fs::create_dir_all(dir.join("migrations")).unwrap();
        for file in files {
            fs::write(dir.join(file), "2\n").unwrap();
        }
        commit(dir, ".");
    };
    let shared = ["version", "migrations/2.sql", "unlisted"];
    change(
        &workspace.path,
        &[&shared[..], &["mine", "feature"]].concat(),
    );
    change(&repo, &shared);
    let check = manager
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert!(!check.conflicts);
    assert_eq!(check.changed_on_both_sides, ["migrations/2.sql", "version"]);
    assert!(!check.clean());
    let recorded = manager
        .spaces
        .recorded_changed_on_both_sides(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (recorded.head, recorded.target_commit, recorded.files),
        (check.head, check.target_commit, check.changed_on_both_sides)
    );
    // Only the default target's result is kept.
    git(&repo, &["branch", "release", "main"]);
    manager
        .spaces
        .check_conflicts(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some("release".into()),
        )
        .await
        .unwrap();
    let recorded = manager.spaces.recorded_changed_on_both_sides(&workspace.id);
    assert_eq!(recorded.await.unwrap().unwrap().target, "main");

    change(&repo, &["mine", "feature"]);
    let check = manager
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    assert!(check.clean(), "the target has HEAD's changes");
    let recorded = manager.spaces.recorded_changed_on_both_sides(&workspace.id);
    assert!(recorded.await.unwrap().unwrap().files.is_empty());
}
