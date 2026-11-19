use super::*;

#[test]
fn issue_names_are_portable_bounded_and_stable() {
    let branch = |title: &str| issue_branch(34, title);
    assert_eq!(
        branch("Fix `API`: $(touch /tmp/no); 日本語"),
        "34-fix-api-touch"
    );
    assert_eq!(
        branch("Remove a merged workspace whose HEAD is a rebased copy"),
        "34-remove-merged-workspace"
    );
    let long = format!("{} done", "x".repeat(200));
    assert_eq!(branch(&long), format!("34-{}", "x".repeat(29)));
    let fits = format!("{} done", "x".repeat(29));
    assert_eq!(branch(&fits), format!("34-{}", "x".repeat(29)));
    assert_eq!(branch(&format!("Fix {}", "x".repeat(200))), "34-fix");
    assert_eq!(branch("日本語 to the"), "issue-34");
}

#[tokio::test]
async fn reading_the_branches_of_a_repository_removed_meanwhile_says_so() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository {
        id: "repo-id".into(),
        path: root.path().join("removed"),
        source: "https://example.com/team/repo".into(),
        last_used: 0,
        name: Some("repo".into()),
        workspaces_dir: root.path().join("workspaces"),
        contribution: false,
    };
    let error = branches(&repo, "racing").await.unwrap_err();
    let reason = crate::refusal::Reason::of(&error);
    assert_eq!(reason, Some(crate::refusal::Reason::NotFound), "{error:#}");
    assert!(
        format!("{error:#}").contains("(repo-id) was removed"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_failed_read_of_a_repository_s_branches_names_it_by_its_id() {
    let root = tempfile::tempdir().unwrap();
    // Its checkout is there, though its Git files are gone, as while a
    // removal deletes them.
    let repo = Repository {
        id: "repo-id".into(),
        path: root.path().to_owned(),
        source: "https://example.com/team/repo".into(),
        last_used: 0,
        name: Some("repo".into()),
        workspaces_dir: root.path().join("workspaces"),
        contribution: false,
    };
    let error = branches(&repo, "racing").await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("repository repo (repo-id)"), "{error}");
}
