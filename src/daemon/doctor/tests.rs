use super::*;
use std::fs;

#[test]
fn unavailable_dependencies_have_the_same_names_as_daemon_checks() {
    let checked: Vec<_> = dependencies(None)
        .into_iter()
        .map(|check| check.name)
        .collect();
    let skipped: Vec<_> = unavailable_checks()
        .into_iter()
        .filter(|check| check.name.starts_with("dependency:"))
        .map(|check| check.name)
        .collect();
    assert_eq!(skipped, checked);
}

#[test]
fn worktree_problems_name_untracked_worktrees_and_a_foreign_seed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let tree = |path: PathBuf| git::Worktree {
        path,
        branch: None,
        prunable: false,
        locked: false,
    };
    let seed = crate::seed::Layout::new(&root, "repo-id");
    let owned = OwnedWorktrees {
        tracked: HashSet::from([root.join("tracked")]),
        seed,
        verified_seed: false,
    };
    let trees = [
        tree(root.join(".checkout")),
        tree(root.join("tracked")),
        tree(root.join("stray")),
        tree(dir.path().join("elsewhere")),
    ];
    let checkout = root.join(".checkout");
    assert_eq!(
        owned.problems(&checkout, &root, &trees),
        [format!(
            "Git worktree is not tracked by Shoal: {}",
            root.join("stray").display()
        )]
    );
    let checks = worktree_report("worktrees:repo".to_owned(), &root, Vec::new());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, CheckStatus::Ok);

    let seed_path = owned.seed.worktree();
    fs::create_dir_all(&seed_path).unwrap();
    let trees = [tree(seed_path.clone())];
    let problems = owned.problems(&checkout, &root, &trees);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].starts_with("Seed path is not Shoal's seed worktree"));
    let verified = OwnedWorktrees {
        verified_seed: true,
        ..owned
    };
    assert!(verified.problems(&checkout, &root, &trees).is_empty());
    let checks = worktree_report("worktrees:repo".to_owned(), &root, problems);
    assert!(
        checks
            .iter()
            .all(|check| check.status == CheckStatus::Warning)
    );
}
