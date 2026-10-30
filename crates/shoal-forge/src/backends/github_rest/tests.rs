use super::*;

#[test]
fn statuses_report_failed_and_running_checks() {
    let states = |checks: Vec<CiCheck>| checks.into_iter().map(|c| c.1).collect::<Vec<_>>();
    let forgejo = json!({"total_count": 2, "statuses": [
        {"context": "ci / rust", "status": "error", "target_url": "/t/r/actions/runs/1/jobs/0"},
        {"context": "ci / docs", "status": "pending"}
    ]});
    let listed = commit_statuses(&forgejo, Dialect::Forgejo).unwrap();
    assert_eq!(listed[0].2, "/t/r/actions/runs/1/jobs/0");
    assert_eq!(states(listed), [JobState::Failed, JobState::Running]);
    let none = json!({"total_count": 0, "statuses": null});
    assert!(commit_statuses(&none, Dialect::Forgejo).unwrap().is_empty());
    let github = json!({"total_count": 1, "statuses": [
        {"context": "deploy", "state": "failure", "target_url": "https://ci.example/1"}
    ]});
    assert_eq!(
        states(commit_statuses(&github, Dialect::GitHub).unwrap()),
        [JobState::Failed]
    );
    assert!(commit_statuses(&github, Dialect::Forgejo).is_err());
}

#[test]
fn dependencies_keep_open_issues() {
    let issues = [
        json!({"state": "open", "html_url": "https://forge.example/o/r/issues/1"}),
        json!({"state": "closed", "html_url": "https://forge.example/o/r/issues/2"}),
    ];
    assert_eq!(
        open_urls(&issues).unwrap(),
        ["https://forge.example/o/r/issues/1"]
    );
}

#[test]
fn forgejo_keeps_inline_comments_and_their_reactions_with_issue_comments() {
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let endpoint = |repo, subject, dialect| reactions_endpoint(repo, 4, subject, dialect);
    assert_eq!(
        endpoint(&github, Subject::Item, Dialect::GitHub),
        "repos/team/repo/issues/4/reactions"
    );
    assert_eq!(
        endpoint(&github, Subject::Comment(9), Dialect::GitHub),
        "repos/team/repo/issues/comments/9/reactions"
    );
    assert_eq!(
        endpoint(&github, Subject::ReviewComment(9), Dialect::GitHub),
        "repos/team/repo/pulls/comments/9/reactions"
    );
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert!(
        endpoint(&forgejo, Subject::ReviewComment(9), Dialect::Forgejo)
            .ends_with("issues/comments/9/reactions")
    );
    // Comment edits go to the same comments.
    let edit = |subject, dialect| edit_comment(&github, subject, "Why?", dialect).unwrap();
    assert_eq!(
        edit(Subject::ReviewComment(9), Dialect::GitHub),
        Request::new(
            HttpMethod::Patch,
            "repos/team/repo/pulls/comments/9".into(),
            Some(json!({"body": "Why?"})),
        )
    );
    assert_eq!(
        edit(Subject::ReviewComment(9), Dialect::Forgejo).endpoint,
        "repos/team/repo/issues/comments/9"
    );
    assert!(edit_comment(&github, Subject::Item, "Why?", Dialect::GitHub).is_err());
    let reaction = json!({"id": 2, "content": "eyes", "user": {"login": "ann"}});
    let listed = listed_reaction(&reaction).unwrap();
    assert_eq!(
        (listed.content.as_str(), listed.login.as_str()),
        ("eyes", "ann")
    );
    assert!(listed_reaction(&json!({"user": null})).is_err());
}
