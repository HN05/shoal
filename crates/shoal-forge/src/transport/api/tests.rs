use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
};

#[tokio::test]
async fn repeated_reads_send_the_validators_and_keep_the_unchanged_body() {
    let server = Server::start(|request| match request.header("If-None-Match") {
        Some("\"v1\"") => (304, Vec::new(), Vec::new()),
        _ => (200, vec!["ETag: \"v1\"".into()], r#"{"number":7}"#.into()),
    });
    let memory = crate::transport::memory::Memory::default();
    let account = || agent_on(&server, "agent").remembering(memory.clone());
    let repo = repo_on(&server, ForgeKind::GitHub, "team/project");
    let read = Request::get("repos/team/project/issues/7".into());
    for _ in 0..2 {
        let body = repo.send(&account(), &read).await.unwrap();
        assert_eq!(body, serde_json::json!({"number": 7}));
    }
    let received = server.received();
    assert_eq!(received.len(), 2, "{received:?}");
    assert_eq!(received[0].line, "GET /api/v3/repos/team/project/issues/7");
    assert_eq!(received[0].header("Authorization"), Some("Bearer agent"));
    assert_eq!(received[0].header("If-None-Match"), None);
    assert_eq!(received[1].header("If-None-Match"), Some("\"v1\""));
    // A change is not conditional, and an account without the memory
    // reads in full.
    let edit = Request::new(HttpMethod::Patch, read.endpoint.clone(), None);
    repo.send(&account(), &edit).await.unwrap();
    repo.send(&agent_on(&server, "agent"), &read).await.unwrap();
    let received = server.received();
    assert!(
        received[2..]
            .iter()
            .all(|request| request.header("If-None-Match").is_none()),
        "{received:?}"
    );
}

#[tokio::test]
async fn reads_are_sent_again_after_a_failure_that_may_pass_and_changes_are_not() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let sent = AtomicUsize::new(0);
    // Every other request finds the forge busy, starting with the first.
    let server = Server::start(move |_| match sent.fetch_add(1, Ordering::SeqCst) % 2 {
        0 => (503, Vec::new(), Vec::new()),
        _ => (200, Vec::new(), r#"{"number":7}"#.into()),
    });
    let account = agent_on(&server, "agent");
    let repo = repo_on(&server, ForgeKind::GitHub, "team/project");
    let endpoint = "repos/team/project/issues/7".to_owned();
    let read = repo.send(&account, &Request::get(endpoint.clone())).await;
    assert_eq!(read.unwrap(), serde_json::json!({"number": 7}));
    let edit = Request::new(HttpMethod::Patch, endpoint, None);
    assert!(repo.send(&account, &edit).await.is_err());
    assert_eq!(server.received().len(), 3);
}

#[test]
fn forgejo_responses_report_status_and_messages() {
    assert_eq!(curl_body("{\"number\":7}\n201").unwrap(), "{\"number\":7}");
    assert_eq!(curl_body("\n204").unwrap(), "");
    let error = curl_body("{\"message\":\"user does not exist\"}\n404").unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 404: user does not exist");
    let error = curl_body("<html>busy</html>\n502").unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 502: <html>busy</html>");
    assert_eq!(format!("{:#}", curl_body("\n404").unwrap_err()), "HTTP 404");
    assert!(curl_body("{}").is_err());
}

#[test]
fn an_oversized_response_names_the_limit_and_a_timeout_is_transient() {
    use std::io::{Error, ErrorKind};
    let oversized = curl_unrun(Error::from(ErrorKind::FileTooLarge), MAX_RESPONSE_BYTES);
    assert!(format!("{oversized:#}").contains("64 MiB"));
    let slow = curl_unrun(Error::from(ErrorKind::TimedOut), 4);
    assert!(slow.downcast_ref::<Transient>().is_some());
}

#[test]
fn encoding_keeps_unreserved_characters() {
    assert_eq!(encode("feat/a b#1", true), "feat/a%20b%231");
    assert_eq!(encode("feat/ü", false), "feat%2F%C3%BC");
}

#[test]
fn github_errors_name_each_validation_failure() {
    let body = r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists"},"base invalid"]}"#;
    assert_eq!(
        error_message(body).unwrap(),
        "Validation Failed: A pull request already exists; base invalid"
    );
    assert_eq!(error_message("not json"), None);
    let gitlab = r#"{"message":{"title":["can't be blank","is too short"],"base":"bad"}}"#;
    assert_eq!(
        error_message(gitlab).unwrap(),
        "base: \"bad\"; title can't be blank, is too short"
    );
    assert_eq!(
        error_message(r#"{"error":"insufficient_scope"}"#).unwrap(),
        "insufficient_scope"
    );
    assert_eq!(
        error_message(r#"{"type":"error","error":{"message":"Repository not found"}}"#).unwrap(),
        "Repository not found"
    );
}
