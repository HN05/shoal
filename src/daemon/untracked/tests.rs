use super::*;
use std::fs;

#[test]
fn problems_name_untracked_worktrees_and_a_foreign_seed() {
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
        owned
            .untracked(&checkout, &root, &trees)
            .collect::<Vec<_>>(),
        [root.join("stray").as_path()]
    );
    assert_eq!(
        owned.problems(&checkout, &root, &trees),
        [format!(
            "Git worktree is not tracked by Shoal: {}",
            root.join("stray").display()
        )]
    );

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
}
