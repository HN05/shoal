use super::*;
use crate::forge::numbers::IssueNumber;
use anyhow::Result;

async fn repository_for<'a>(repos: &'a [Repository], url: &str) -> Result<&'a Repository> {
    let issue = ItemRef::parse_kind(LinkKind::Issue, url)?;
    repository_with_remote(repos, issue.repository().clone(), "retry").await
}

#[test]
fn forge_urls_match_transports_and_reject_other_repositories() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert_eq!(
        repo,
        ForgeRepo::parse("ssh://git@github.com:2222/team/repo").unwrap()
    );
    let issue = |input: &str| {
        let input = input.parse::<ItemInput>()?;
        ItemRef::in_repository(&repo, LinkKind::Issue, &input)?.issue()
    };
    assert_eq!(issue("42").unwrap(), IssueNumber::new(42));
    assert_eq!(
        issue("https://github.com/team/repo/issues/42#comment").unwrap(),
        IssueNumber::new(42)
    );
    for input in [
        "0",
        "-1",
        "--help",
        "https://other.test/team/repo/issues/42",
        "https://github.com/team/other/issues/42",
        "https://github.com/team/repo/pull/42",
    ] {
        assert!(issue(input).is_err(), "{input}");
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
    let pull: ItemRef = "https://code.example/group/sub/repo/-/merge_requests/3"
        .parse()
        .unwrap();
    let pull = repository_with_remote(&repos, pull.repository().clone(), "retry").await;
    assert_eq!(pull.unwrap().id, "d");
    assert!(
        repository_for(&repos, "https://forge.example/team/repo/issues/7")
            .await
            .is_err()
    );
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
        assert!(
            error.contains(&format!("`shoal repo add {repository}`")),
            "{error}"
        );
    }
}

#[test]
fn issue_names_are_portable_bounded_and_stable() {
    let mut issue = Issue {
        number: 34,
        title: "Fix `API`: $(touch /tmp/no); 日本語".into(),
        url: "https://forge.example/team/repo/issues/34".parse().unwrap(),
        details: "body".into(),
        labels: Vec::new(),
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
    let subject = issue.subject();
    assert!(
        (subject.text).contains("#34: 日本語 to the\nhttps://forge.example/team/repo/issues/34\n"),
        "{}",
        subject.text
    );
    assert!(
        (subject.text).contains("Issue description:\nbody\n"),
        "{}",
        subject.text
    );
    assert_eq!(subject.issue.unwrap().number, 34);
    issue.details = " \n".into();
    assert!(!issue.subject().text.contains("Issue description"));
}
