use super::*;

async fn repository_for<'a>(repos: &'a [Repository], url: &str) -> Result<&'a Repository> {
    repository_with_remote(repos, ForgeRepo::from_issue_url(url)?, "retry").await
}

#[test]
fn forge_urls_match_transports_and_reject_other_repositories() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert_eq!(
        repo,
        ForgeRepo::parse("ssh://git@github.com:2222/team/repo").unwrap()
    );
    assert_eq!(repo.issue("42").unwrap().0, 42);
    assert_eq!(
        repo.issue("https://github.com/team/repo/issues/42#comment")
            .unwrap()
            .0,
        42
    );
    for input in [
        "0",
        "-1",
        "--help",
        "https://other.test/team/repo/issues/42",
        "https://github.com/team/other/issues/42",
        "https://github.com/team/repo/pull/42",
    ] {
        assert!(repo.issue(input).is_err(), "{input}");
    }
    assert!(ForgeRepo::parse("/tmp/repo").is_err());
}

#[tokio::test]
async fn issue_urls_select_exactly_one_registered_repository() {
    let repo = |id: &str, source: &str| Repository {
        id: id.into(),
        path: format!("/nonexistent/{id}").into(),
        source: source.into(),
        last_used: 0,
        name: None,
        workspaces_dir: None,
        contribution: false,
    };
    let repos = [
        repo("a", "https://github.com/team/repo.git"),
        repo("b", "git@forge.example:team/repo.git"),
        repo("c", "ssh://git@forge.example:2222/team/repo"),
        repo("d", "git@code.example:group/sub/repo.git"),
    ];
    let url = "https://github.com/team/repo/issues/7#issuecomment-1";
    assert_eq!(repository_for(&repos, url).await.unwrap().id, "a");
    // A nested GitLab project matches by its link's routes, before its
    // host is resolved.
    let url = "https://code.example/group/sub/repo/-/issues/7";
    assert_eq!(repository_for(&repos, url).await.unwrap().id, "d");
    let pull = ForgeRepo::from_pull_url("https://code.example/group/sub/repo/-/merge_requests/3");
    let pull = repository_with_remote(&repos, pull.unwrap(), "retry").await;
    assert_eq!(pull.unwrap().id, "d");
    let error = repository_for(&repos, "https://forge.example/team/repo/issues/7")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("several"), "{error}");
    let error = repository_for(&repos, "https://github.com/team/other/issues/7")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("shoal repo add https://github.com/team/other"),
        "{error}"
    );
    assert!(
        repository_for(&repos, "https://github.com/team/repo/pull/7")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_repositories_suggest_the_repository_url() {
    for (url, repository) in [
        (
            "https://github.com/team/repo/issues/7#issuecomment-1",
            "https://github.com/team/repo",
        ),
        (
            "http://FORGE.example:3000/team/Repo.git/issues/7/?query#comment",
            "http://forge.example:3000/team/Repo",
        ),
        (
            "https://user:secret@forge.example/team/repo/issues/7",
            "https://forge.example/team/repo",
        ),
    ] {
        let error = repository_for(&[], url).await.unwrap_err().to_string();
        assert_eq!(
            error,
            format!(
                "no registered repository has the remote {}; register it with `shoal repo add {repository}` and retry",
                repository.split_once("://").unwrap().1,
            )
        );
    }
}

#[test]
fn issue_names_are_portable_bounded_and_stable() {
    let mut issue = Issue {
        number: 34,
        title: "Fix `API`: $(touch /tmp/no); 日本語".into(),
        url: "url".into(),
        details: "body".into(),
    };
    assert_eq!(issue.branch_name(), "34-fix-api-touch");
    issue.title = "Remove a merged workspace whose HEAD is a rebased copy".into();
    assert_eq!(issue.branch_name(), "34-remove-merged-workspace");
    issue.title = format!("{} done", "x".repeat(200));
    assert_eq!(issue.branch_name(), format!("34-{}", "x".repeat(29)));
    issue.title = format!("{} done", "x".repeat(29));
    assert_eq!(issue.branch_name(), format!("34-{}", "x".repeat(29)));
    issue.title = format!("Fix {}", "x".repeat(200));
    assert_eq!(issue.branch_name(), "34-fix");
    issue.title = "日本語 to the".into();
    assert_eq!(issue.branch_name(), "issue-34");
    assert!(
        issue
            .prompt()
            .contains("url\n\nIssue description:\nbody\n\n`shoal view`")
    );
    issue.details = " \n".into();
    assert!(!issue.prompt().contains("Issue description"));
}
