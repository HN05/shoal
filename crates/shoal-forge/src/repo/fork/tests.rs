use super::*;
use crate::account::{AgentLogins, Launch};
use crate::test_support::install_test_script;
use std::path::PathBuf;
use tokio::process::Command;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

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
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let gh = dir.path().join("gh");
    install_test_script(
        &gh,
        &format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case "$*" in
  *forks*) printf 'HTTP/2.0 202 Accepted\n\n{{"full_name":"someone/repo"}}';;
  *) printf 'HTTP/2.0 200 OK\n\n{{"full_name":"team/repo","permissions":{{"push":false}}}}';;
esac
"#,
            log = log.display()
        ),
    );
    let logins = AgentLogins {
        launch: Box::new(Bin(dir.path().to_owned())),
        logins: Default::default(),
        separate: Vec::new(),
    };
    let account = Account::agent(logins, Default::default());
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert!(!repo.pushable(&account).await.unwrap());
    let fork = repo.fork(&account).await.unwrap();
    assert_eq!(
        fork,
        ForgeRepo::parse("https://github.com/someone/repo").unwrap()
    );
    let log = std::fs::read_to_string(log).unwrap();
    let lines: Vec<_> = log.lines().collect();
    assert!(lines[0].contains("repos/team/repo"), "{log}");
    assert!(
        lines[1].contains("POST") && lines[1].contains("repos/team/repo/forks"),
        "{log}"
    );
}
