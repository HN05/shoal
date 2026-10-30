use super::*;

#[test]
fn issue_input_classification_defers_validation_until_lookup() {
    let repo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    for input in ["0", "18446744073709551616"] {
        assert_eq!(IssueInput::parse(input), IssueInput::Number);
        assert!(repo.issue(input).is_err());
    }
    for input in ["", "-1", "+1", "#1", "1/", "１２", "not-an-issue"] {
        assert_eq!(IssueInput::parse(input), IssueInput::Invalid);
        assert!(repo.issue(input).is_err());
    }
    for input in [
        "https://forge.example/team/repo/issues/1?x#comment",
        "http://forge.example/team/repo/issues/1/",
    ] {
        assert_eq!(IssueInput::parse(input), IssueInput::Url);
        assert_eq!(repo.issue(input).unwrap().0, 1);
    }
    for input in [
        "https://forge.example/team/repo/pulls/1",
        "https://forge.example/team/repo/issues/0",
        "https://forge.example/team/other/issues/1",
    ] {
        assert_eq!(IssueInput::parse(input), IssueInput::Url);
        assert!(repo.issue(input).is_err());
    }
    assert_eq!(IssueInput::parse("001"), IssueInput::Number);
    assert_eq!(repo.issue("001").unwrap().0, 1);
}

#[test]
fn backend_is_selected_from_the_normalized_remote_host() {
    for (remote, kind) in [
        ("git@GitHub.COM:team/repo.git", ForgeKind::GitHub),
        ("ssh://git@github.com:2222/team/repo", ForgeKind::GitHub),
        ("https://forge.example/team/repo.git", ForgeKind::Forgejo),
        ("http://forge.example:3000/team/repo", ForgeKind::Forgejo),
        ("git@gitlab.com:group/sub/project.git", ForgeKind::GitLab),
        ("git@bitbucket.org:workspace/repo.git", ForgeKind::Bitbucket),
    ] {
        assert_eq!(ForgeRepo::parse(remote).unwrap().kind, kind);
    }
}

#[test]
fn bitbucket_items_use_pull_request_routes() {
    let repo = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    let base = "https://bitbucket.org/workspace/repo";
    assert_eq!(repo.issue("4").unwrap(), (4, format!("{base}/issues/4")));
    let url = format!("{base}/pull-requests/5");
    assert_eq!(repo.pull("5").unwrap(), (5, url.clone()));
    assert_eq!(
        ForgeRepo::from_pull_url(&format!("{url}/diff")).unwrap(),
        repo
    );
    assert!(repo.pull(&format!("{base}/pulls/5")).is_err());
    assert_eq!(
        repo.repo_endpoint("pullrequests/5"),
        "repositories/workspace/repo/pullrequests/5"
    );
}

#[test]
fn only_gitlab_paths_nest_groups() {
    let repo = ForgeRepo::parse("https://gitlab.com/group/sub/project.git").unwrap();
    assert_eq!(repo.path, "group/sub/project");
    assert_eq!(
        repo.repo_endpoint("issues/3"),
        "projects/group%2Fsub%2Fproject/issues/3"
    );
    for remote in [
        "https://github.com/group/sub/repo",
        "https://forge.example/group/sub/repo",
        "https://gitlab.com/project",
        "https://gitlab.com/group/-/project",
        "https://gitlab.com/group//project",
    ] {
        assert!(ForgeRepo::parse(remote).is_err(), "{remote}");
    }
}

#[test]
fn gitlab_items_use_their_own_routes() {
    let repo = ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap();
    let base = "https://gitlab.com/group/sub/project";
    assert_eq!(repo.issue("4").unwrap(), (4, format!("{base}/-/issues/4")));
    assert_eq!(
        repo.pull(&format!("{base}/-/merge_requests/5/diffs"))
            .unwrap_err()
            .to_string(),
        "invalid PR number"
    );
    assert_eq!(
        repo.pull(&format!("{base}/-/merge_requests/5#note_1"))
            .unwrap(),
        (5, format!("{base}/-/merge_requests/5"))
    );
    for (url, kind) in [
        (format!("{base}/-/issues/4"), "issue"),
        (format!("{base}/-/merge_requests/5"), "pr"),
    ] {
        let discovered = if kind == "issue" {
            ForgeRepo::from_issue_url(&url)
        } else {
            ForgeRepo::from_pull_url(&url)
        };
        assert_eq!(discovered.unwrap(), repo, "{url}");
    }
    assert!(repo.issue(&format!("{base}/issues/4")).is_err());
    assert!(repo.pull(&format!("{base}/pulls/5")).is_err());
    assert!(
        repo.pull(&format!("{base}/-/merge_requests/5/../6"))
            .is_err()
    );
}

#[test]
fn pr_identity_is_strict() {
    let repo = ForgeRepo::parse("git@example.com:team/repo.git").unwrap();
    assert_eq!(
        repo.pull("https://example.com/team/repo/pulls/56").unwrap(),
        (56, "https://example.com/team/repo/pulls/56".into())
    );
    for url in [
        "https://example.com/other/repo/pulls/56",
        "https://other.com/team/repo/pulls/56",
        "https://example.com/team/repo/pulls/0",
    ] {
        assert!(repo.pull(url).is_err());
    }
}

/// Every host serves Forgejo.
struct Forgejo;

impl HostTypes for Forgejo {
    async fn forge_type(&self, _host: &str, _scheme: &'static str) -> Result<HostType> {
        Ok(HostType::Settled(ForgeKind::Forgejo))
    }
}

#[tokio::test]
async fn a_named_repository_is_a_url_or_a_path_on_the_same_host() {
    let repo = ForgeRepo::parse("git@forge.example:team/repo.git").unwrap();
    for (input, expected) in [
        ("team/other", "https://forge.example/team/other"),
        ("someone/tool.git", "https://forge.example/someone/tool"),
        (
            "https://elsewhere.example/them/tool",
            "https://elsewhere.example/them/tool",
        ),
        (
            "git@elsewhere.example:them/tool.git",
            "https://elsewhere.example/them/tool",
        ),
    ] {
        ForgeRepo::check_named(input).unwrap();
        let named = repo.resolve_named(input, &Forgejo).await.unwrap();
        assert_eq!(named.repository_url(), expected, "{input}");
    }
    for input in ["tool", "team/../tool", "team/tool/extra", ""] {
        assert!(
            repo.resolve_named(input, &Forgejo).await.is_err(),
            "{input}"
        );
    }
    // Before any host answers, only what names no repository on any forge
    // is refused; nested groups wait for the forge.
    ForgeRepo::check_named("group/team/tool").unwrap();
    for input in [
        "tool",
        "team/../tool",
        "",
        "https://forge.example/tool",
        "ftp://x/a/b",
    ] {
        assert!(ForgeRepo::check_named(input).is_err(), "{input}");
    }
}

#[test]
fn pull_urls_name_their_repository() {
    let repo = ForgeRepo::from_pull_url("https://github.com/team/repo/pull/56/files").unwrap();
    assert_eq!(repo, ForgeRepo::parse("git@github.com:team/repo").unwrap());
    assert!(ForgeRepo::from_pull_url("https://github.com/team/repo/issues/56").is_err());
}

#[test]
fn item_locators_share_validation_and_preserve_links() {
    for (remote, base, pull_route) in [
        (
            "git@github.com:team/Repo.git",
            "https://github.com/team/Repo",
            "pull",
        ),
        (
            "ssh://git@forge.example:2222/team/Repo.git",
            "https://forge.example/team/Repo",
            "pulls",
        ),
        (
            "http://forge.example:3000/team/Repo.git",
            "http://forge.example:3000/team/Repo",
            "pulls",
        ),
    ] {
        let repo = ForgeRepo::parse(remote).unwrap();
        for (route, lookup) in [
            (
                "issues",
                ForgeRepo::issue as fn(&ForgeRepo, &str) -> Result<(u64, String)>,
            ),
            (pull_route, ForgeRepo::pull),
        ] {
            for (digits, number) in [("56", 56), ("0056", 56), ("18446744073709551615", u64::MAX)] {
                let url = format!("{base}/{route}/{digits}");
                assert_eq!(lookup(&repo, digits).unwrap(), (number, url.clone()));
                for suffix in [
                    "",
                    "/",
                    "///",
                    "?tab=files",
                    "#comment",
                    "/?tab=files#comment",
                    "#comment?query",
                ] {
                    let input = format!("{url}{suffix}");
                    assert_eq!(
                        lookup(&repo, &input).unwrap(),
                        (number, url.clone()),
                        "{input}"
                    );
                }
                // Supplied links retain their scheme, host case, .git and leading zeroes.
                let spelled = format!(
                    "https://{}/team/Repo.git/{route}/{digits}",
                    repo.host.to_uppercase()
                );
                assert_eq!(lookup(&repo, &spelled).unwrap(), (number, spelled));
            }
            for invalid in [
                "",
                "0",
                "-1",
                "+56",
                "#56",
                "56/",
                "56?x",
                "56#x",
                " 56",
                "56 ",
                "１２",
                "HEAD",
                "main~1",
                "team/Repo#56",
                "18446744073709551616",
            ] {
                assert!(lookup(&repo, invalid).is_err(), "{route}: {invalid}");
                let url = format!("{base}/{route}/{invalid}");
                // URL suffixes are stripped, but bare-number suffixes are invalid.
                if !["56/", "56?x", "56#x"].contains(&invalid) {
                    assert!(lookup(&repo, &url).is_err(), "{url}");
                }
            }
            for url in [
                format!("https://other.example/team/Repo/{route}/56"),
                format!("{base}-other/{route}/56"),
                format!("{base}/{route}/56/files"),
                format!("{base}/{route}/%35%36"),
                format!("ssh://git@{}/{}/{route}/56", repo.host, repo.path),
                format!("git@{}:{}/{route}/56", repo.host, repo.path),
                format!("//{}/{}/{route}/56", repo.host, repo.path),
                format!("HTTPS://{}/{}/{route}/56", repo.host, repo.path),
            ] {
                assert!(lookup(&repo, &url).is_err(), "{url}");
            }
            for wrong_route in ["issues", "pull", "pulls", "commits"] {
                if wrong_route != route {
                    let url = format!("{base}/{wrong_route}/56");
                    assert!(lookup(&repo, &url).is_err(), "{url}");
                }
            }
        }
    }
}

#[test]
fn item_locator_errors_identify_the_item_kind() {
    let repo = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    for (input, issue, pull) in [
        ("0", "issue number must be positive", "invalid PR number"),
        (
            "18446744073709551616",
            "issue number is too large",
            "PR number is too large",
        ),
        (
            "HEAD",
            "issue must be a positive number or an issue URL",
            "invalid PR number",
        ),
        (
            "https://github.com/team/repo",
            "expected an issue URL ending in /issues/<number>",
            "invalid PR URL",
        ),
    ] {
        assert_eq!(repo.issue(input).unwrap_err().to_string(), issue);
        assert_eq!(repo.pull(input).unwrap_err().to_string(), pull);
    }
    for (input, expected) in [
        (
            "https://github.com/other/repo/issues/1",
            "issue URL belongs to a different repository",
        ),
        (
            "https://github.com/other/repo/pull/1",
            "PR belongs to a different repository",
        ),
    ] {
        let error = if input.contains("/issues/") {
            repo.issue(input)
        } else {
            repo.pull(input)
        };
        assert_eq!(error.unwrap_err().to_string(), expected);
    }
}

#[test]
fn item_repository_discovery_defers_number_validation() {
    for (route, discover, lookup) in [
        (
            "issues",
            ForgeRepo::from_issue_url as fn(&str) -> Result<ForgeRepo>,
            ForgeRepo::issue as fn(&ForgeRepo, &str) -> Result<(u64, String)>,
        ),
        ("pull", ForgeRepo::from_pull_url, ForgeRepo::pull),
    ] {
        for tail in [
            "56/?query#fragment",
            "0",
            "18446744073709551616",
            "56/files",
        ] {
            let url = format!("http://GitHub.com/team/Repo/{route}/{tail}");
            let repo = discover(&url).unwrap();
            assert_eq!(
                repo,
                ForgeRepo::parse("git@github.com:team/Repo.git").unwrap()
            );
            assert_eq!(repo.web_scheme, "http");
            assert_eq!(lookup(&repo, &url).is_ok(), tail == "56/?query#fragment");
        }
    }
    let url = "https://forge.example/team/Repo/pulls/56/?query#fragment";
    assert_eq!(
        ForgeRepo::from_pull_url(url).unwrap(),
        ForgeRepo::parse("git@forge.example:team/Repo.git").unwrap()
    );
}
