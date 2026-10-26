use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

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

#[tokio::test]
async fn root_dir_reports_whether_the_daemon_can_write_there() {
    let root = tempfile::tempdir().unwrap();
    let check = root_dir(Ok(root.path().join("shoal"))).await;
    assert_eq!(check.status, CheckStatus::Ok);
    let file = root.path().join("file");
    fs::write(&file, "").unwrap();
    let check = root_dir(Ok(root.path().join("missing/../file"))).await;
    assert_eq!(check.status, CheckStatus::Error);
    let check = root_dir(Ok(file.clone())).await;
    assert_eq!(check.status, CheckStatus::Error);
    assert!(
        check.message.starts_with(&format!(
            "Cannot use root_dir: the Shoal daemon cannot write to {}: ",
            fs::canonicalize(&file).unwrap().display()
        )),
        "{}",
        check.message
    );
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

#[test]
fn dependencies_require_executable_files_on_the_supplied_path() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("git"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(root.path().join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.path().join("wt"), "not executable").unwrap();
    fs::create_dir(root.path().join("lsof")).unwrap();
    let checks = dependencies(Some(root.path().as_os_str()));
    assert_eq!(checks[0].status, CheckStatus::Ok);
    assert_eq!(checks[1].status, CheckStatus::Error);
    assert_eq!(checks[2].status, CheckStatus::Error);
    assert_eq!(checks[3].status, CheckStatus::Error);
    assert_eq!(checks[4].status, CheckStatus::Warning);
    assert!(
        dependencies(None)
            .iter()
            .all(|c| c.status != CheckStatus::Ok)
    );
}
