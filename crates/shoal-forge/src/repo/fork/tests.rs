use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
};

#[test]
fn a_fork_remote_keeps_the_origin_url_form() {
    let fork = ForgeRepo::parse("https://github.com/someone/repo").unwrap();
    for (origin, expected) in [
        (
            "https://github.com/team/repo",
            "https://github.com/someone/repo",
        ),
        (
            "https://github.com/team/repo.git",
            "https://github.com/someone/repo.git",
        ),
        (
            "git@github.com:team/repo.git",
            "git@github.com:someone/repo.git",
        ),
        (
            "ssh://git@github.com:2222/team/repo",
            "ssh://git@github.com:2222/someone/repo",
        ),
    ] {
        assert_eq!(fork.remote_like(origin).unwrap(), expected);
    }
    assert!(fork.remote_like("https://github.com").is_err());
}

#[test]
fn only_forges_that_open_pull_requests_from_forks_fork() {
    for (remote, forks) in [
        ("https://github.com/team/repo", true),
        ("https://forge.example/team/repo", true),
        ("https://gitlab.com/team/repo", false),
        ("https://bitbucket.org/team/repo", false),
    ] {
        let repo = ForgeRepo::parse(remote).unwrap();
        assert_eq!(repo.require(Capability::Forks).is_ok(), forks, "{remote}");
    }
}

#[tokio::test]
async fn github_reads_push_access_and_forks_into_the_account() {
    let server = Server::start(|request| match request.line.as_str() {
        line if line.ends_with("/forks") => {
            (202, Vec::new(), r#"{"full_name":"someone/repo"}"#.into())
        }
        _ => (
            200,
            Vec::new(),
            r#"{"full_name":"team/repo","permissions":{"push":false}}"#.into(),
        ),
    });
    let account = agent_on(&server, "agent");
    let repo = repo_on(&server, ForgeKind::GitHub, "team/repo");
    assert!(!repo.pushable(&account).await.unwrap());
    let fork = repo.fork(&account).await.unwrap();
    assert_eq!(
        (fork.host.as_str(), fork.path.as_str()),
        (server.host.as_str(), "someone/repo")
    );
    let lines: Vec<_> = server
        .received()
        .into_iter()
        .map(|request| request.line)
        .collect();
    assert_eq!(
        lines,
        [
            "GET /api/v3/repos/team/repo",
            "POST /api/v3/repos/team/repo/forks"
        ]
    );
}
