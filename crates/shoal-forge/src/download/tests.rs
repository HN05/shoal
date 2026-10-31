use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Reply, Server, agent_on, repo_on},
};

/// A GitHub on a loopback port that answers every request with `reply`,
/// one of its repositories, and the URL of a file it serves.
fn github_replying(reply: Reply) -> (Server, ForgeRepo, String) {
    let server = Server::start(move |_| reply.clone());
    let repo = repo_on(&server, ForgeKind::GitHub, "team/project");
    let url = format!("http://{}/user-attachments/assets/abc", server.host);
    (server, repo, url)
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
    let png = b"\x89PNG\r\n\x1a\n\xff".to_vec();
    let content = vec!["Content-Type: Image/PNG; charset=binary".to_owned()];
    let (server, repo, url) = github_replying((200, content, png.clone()));
    let download = repo
        .download(&agent_on(&server, "agent"), &url)
        .await
        .unwrap();
    assert_eq!(download.content_type.as_deref(), Some("image/png"));
    assert_eq!(download.bytes, png);
    let received = server.received();
    assert_eq!(received[0].line, "GET /user-attachments/assets/abc");
    assert_eq!(received[0].header("Authorization"), Some("Bearer agent"));
}

#[tokio::test]
async fn a_sign_in_page_or_failure_is_not_a_download() {
    let html = vec!["Content-Type: text/html; charset=utf-8".to_owned()];
    let (server, repo, url) = github_replying((200, html, b"<html>Sign in</html>".to_vec()));
    let error = repo
        .download(&agent_on(&server, "agent"), &url)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("web page"), "{error:#}");
    let json = vec!["Content-Type: application/json".to_owned()];
    let missing = br#"{"message":"Not Found"}"#.to_vec();
    let (server, repo, url) = github_replying((404, json, missing));
    let error = repo
        .download(&agent_on(&server, "agent"), &url)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("HTTP 404: Not Found"),
        "{error:#}"
    );
}

#[tokio::test]
async fn gitlab_reads_an_upload_from_its_api_route() {
    let png = vec!["Content-Type: image/png".to_owned()];
    let server = Server::start(move |_| (200, png.clone(), b"PNG".to_vec()));
    let repo = repo_on(&server, ForgeKind::GitLab, "team/project");
    let url = format!(
        "http://{}/-/project/42/uploads/0123abcd/shot.png",
        server.host
    );
    let download = repo
        .download(&agent_on(&server, "agent"), &url)
        .await
        .unwrap();
    assert_eq!(download.content_type.as_deref(), Some("image/png"));
    assert_eq!(download.bytes, b"PNG");
    let received = server.received();
    assert_eq!(
        received[0].line,
        "GET /api/v4/projects/42/uploads/0123abcd/shot.png"
    );
    assert_eq!(received[0].header("Authorization"), Some("Bearer agent"));
}

#[tokio::test]
async fn forges_without_downloads_refuse_before_sending() {
    let repo = ForgeRepo::parse("https://bitbucket.org/team/project").unwrap();
    let error = repo
        .download(
            &Account::default(),
            "https://bitbucket.org/repo/abc/images/1-shot.png",
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
    let limited = vec!["Retry-After: 30".to_owned()];
    let (server, repo, url) = github_replying((429, limited, Vec::new()));
    let memory = crate::transport::memory::Memory::default();
    // A refusal records the limit, and the next download waits it out
    // without asking the forge again.
    for asked in [1, 1] {
        let account = agent_on(&server, "agent").remembering(memory.clone());
        let error = repo.download(&account, &url).await.unwrap_err();
        assert!(
            crate::transport::limits::rate_limited(&error).is_some(),
            "{error:#}"
        );
        assert_eq!(server.received().len(), asked);
    }
}
