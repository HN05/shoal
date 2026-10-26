use super::*;

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
fn worktree_report_passes_without_problems_and_warns_for_each() {
    let root = Path::new("/repo");
    let checks = worktree_report("worktrees:repo".to_owned(), root, Vec::new());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, CheckStatus::Ok);
    let problems = vec!["one".to_owned(), "two".to_owned()];
    let checks = worktree_report("worktrees:repo".to_owned(), root, problems);
    assert_eq!(checks.len(), 2);
    assert!(
        checks
            .iter()
            .all(|check| check.status == CheckStatus::Warning)
    );
}
