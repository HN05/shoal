use crate::launches::workspace::branch::*;
use crate::model::Repository;

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
        shared_url: None,
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
        shared_url: None,
    };
    let error = branches(&repo, "racing").await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("repository repo (repo-id)"), "{error}");
}
