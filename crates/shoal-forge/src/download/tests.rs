use std::path::PathBuf;

use tokio::process::Command;

use super::*;
use crate::account::{AgentLogins, Launch};
use crate::test_support::install_test_script;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

/// An account whose gh prints `response` after logging its arguments.
fn gh_printing(dir: &std::path::Path, response: &str) -> Account {
    install_test_script(
        &dir.join("gh"),
        &format!(
            "#!/bin/sh\necho \"$*\" > '{log}'\nprintf '{response}'\n",
            log = dir.join("log").display()
        ),
    );
    let logins = AgentLogins {
        launch: Box::new(Bin(dir.to_owned())),
        fj_home: None,
        fj: false,
        tea: false,
    };
    Account::agent(logins, &Default::default())
}

#[test]
fn only_urls_on_the_forge_are_downloaded() {
    let github = ForgeRepo::parse("https://github.com/team/project").unwrap();
    for url in [
        "https://github.com/user-attachments/assets/abc",
        "https://GitHub.com/team/project/raw/main/shot.png",
    ] {
        github.check_download(url).unwrap();
    }
    for url in [
        "https://github.com.example.com/shot.png",
        "https://github.com@example.com/shot.png",
        "https://api.github.com/user",
        "http://github.com/user-attachments/assets/abc",
        "/user-attachments/assets/abc",
        "https://github.com/a b",
    ] {
        assert!(github.check_download(url).is_err(), "{url}");
    }
    for url in ["not-a-url", "ftp://github.com/a", "https:///a", "https://"] {
        assert!(check_url(url).is_err(), "{url}");
    }
    check_url("http://example.com/shot.png").unwrap();
    // A forge served over HTTP links its files over HTTP too.
    let local = ForgeRepo::parse("http://git.local:3000/team/project.git").unwrap();
    local
        .check_download("http://git.local:3000/attachments/abc")
        .unwrap();
    assert!(
        local
            .check_download("http://git.local/attachments/abc")
            .is_err()
    );
}

#[tokio::test]
async fn a_download_keeps_the_bytes_and_their_type() {
    let dir = tempfile::tempdir().unwrap();
    let account = gh_printing(
        dir.path(),
        "HTTP/2.0 200 OK\\r\\nContent-Type: Image/PNG; charset=binary\\r\\n\\r\\n\\211PNG\\r\\n\\032\\n\\377",
    );
    let repo = ForgeRepo::parse("https://github.com/team/project").unwrap();
    let url = "https://github.com/user-attachments/assets/abc";
    let download = repo.download(&account, url).await.unwrap();
    assert_eq!(download.content_type.as_deref(), Some("image/png"));
    assert_eq!(download.bytes, b"\x89PNG\r\n\x1a\n\xff");
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
    assert_eq!(log.trim(), format!("api --include {url}"));
}

#[tokio::test]
async fn a_sign_in_page_or_failure_is_not_a_download() {
    let dir = tempfile::tempdir().unwrap();
    let repo = ForgeRepo::parse("https://github.com/team/project").unwrap();
    let url = "https://github.com/user-attachments/assets/abc";
    let account = gh_printing(
        dir.path(),
        "HTTP/2.0 200 OK\\nContent-Type: text/html; charset=utf-8\\n\\n<html>Sign in</html>",
    );
    let error = repo.download(&account, url).await.unwrap_err();
    assert!(format!("{error:#}").contains("web page"), "{error:#}");
    let account = gh_printing(
        dir.path(),
        "HTTP/2.0 404 Not Found\\nContent-Type: application/json\\n\\n{\"message\":\"Not Found\"}",
    );
    let error = repo.download(&account, url).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("HTTP 404: Not Found"),
        "{error:#}"
    );
}

#[tokio::test]
async fn forges_without_downloads_refuse_before_sending() {
    let repo = ForgeRepo::parse("https://gitlab.com/team/project").unwrap();
    let error = repo
        .download(
            &Account::default(),
            "https://gitlab.com/team/project/uploads/a/b.png",
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("download images and attached files"),
        "{error:#}"
    );
}

#[tokio::test]
async fn downloads_keep_to_the_forges_rate_limit() {
    let dir = tempfile::tempdir().unwrap();
    let memory = crate::transport::memory::Memory::default();
    let account = || {
        gh_printing(
            dir.path(),
            "HTTP/2.0 429 Too Many Requests\\nRetry-After: 30\\n\\n",
        )
        .remembering(memory.clone())
    };
    let repo = ForgeRepo::parse("https://github.com/team/project").unwrap();
    let url = "https://github.com/user-attachments/assets/abc";
    // A refusal records the limit, and the next download waits it out
    // without asking the forge again.
    let log = dir.path().join("log");
    for asked in [true, false] {
        let error = repo.download(&account(), url).await.unwrap_err();
        assert!(
            crate::transport::limits::rate_limited(&error).is_some(),
            "{error:#}"
        );
        assert_eq!(std::fs::remove_file(&log).is_ok(), asked);
    }
}
