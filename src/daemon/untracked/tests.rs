use super::*;

#[test]
fn untracked_leaves_out_the_checkout_tracked_worktrees_and_the_verified_seed() {
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
    // Shoal's seed is not untracked once it is verified as Shoal's.
    let seed_path = owned.seed.worktree();
    let trees = [tree(seed_path.clone())];
    assert_eq!(
        owned.untracked(&checkout, &root, &trees).count(),
        1,
        "an unverified seed path counts as untracked"
    );
    let verified = OwnedWorktrees {
        verified_seed: true,
        ..owned
    };
    assert_eq!(verified.untracked(&checkout, &root, &trees).count(), 0);
}
