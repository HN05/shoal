use super::*;

/// A submission of `drafts` and `requests`, each a POST to its endpoint.
fn submission(drafts: &[&str], requests: &[&str]) -> Submission {
    let post = |endpoint: &&str| {
        Request::new(
            shoal_forge_types::raw::HttpMethod::Post,
            (*endpoint).into(),
            None,
        )
    };
    Submission {
        drafts: drafts.iter().map(post).collect(),
        requests: requests.iter().map(post).collect(),
    }
}

/// A forge that fails the request to `failing` and records the requests
/// it receives, in order.
struct Scripted {
    failing: &'static str,
    sent: std::cell::RefCell<Vec<String>>,
}

impl Exchange for Scripted {
    async fn exchange(&self, request: &Request) -> Result<Value> {
        let line = format!("{} {}", request.method.as_str(), request.endpoint);
        self.sent.borrow_mut().push(line);
        anyhow::ensure!(request.endpoint != self.failing, "{} failed", self.failing);
        Ok(serde_json::json!({ "id": self.sent.borrow().len() }))
    }

    fn discard(&self, draft: &Value) -> Result<Request> {
        let endpoint = format!("drafts/{}", draft["id"]);
        Ok(Request::new(
            shoal_forge_types::raw::HttpMethod::Delete,
            endpoint,
            None,
        ))
    }
}

/// Send `submission`, failing the request to `failing`, and return what it
/// left with the requests sent, in order.
async fn send(submission: Submission, failing: &'static str) -> (Submitted, Vec<String>) {
    let forge = Scripted {
        failing,
        sent: Default::default(),
    };
    let submitted = submission.send(&forge).await;
    (submitted, forge.sent.into_inner())
}

#[tokio::test]
async fn a_failure_before_publishing_deletes_the_drafts_created() {
    let (submitted, sent) = send(submission(&["d", "e", "f"], &["publish"]), "e").await;
    assert_eq!(sent, ["POST d", "POST e", "DELETE drafts/1"]);
    assert!(!submitted.published);
    assert_eq!(submitted.failure.unwrap().to_string(), "e failed");

    let (submitted, sent) = send(submission(&["d"], &["publish", "approve"]), "publish").await;
    assert_eq!(sent, ["POST d", "POST publish", "DELETE drafts/1"]);
    assert!(!submitted.published);
}

#[tokio::test]
async fn a_failure_after_publishing_keeps_what_was_posted() {
    let (submitted, sent) = send(submission(&["d"], &["publish", "approve"]), "approve").await;
    assert_eq!(sent, ["POST d", "POST publish", "POST approve"]);
    assert!(submitted.published);
    assert_eq!(submitted.responses.len(), 2);
    assert!(submitted.failure.is_some());

    let (submitted, sent) = send(submission(&[], &["a", "b"]), "none").await;
    assert_eq!(sent, ["POST a", "POST b"]);
    assert!(submitted.published && submitted.failure.is_none());
}

#[tokio::test]
async fn submissions_to_one_pr_take_turns_and_leave_no_lock_behind() {
    use futures_util::FutureExt;
    let url = "https://gitlab.example/g/r/-/merge_requests/1";
    let first = Submitting::start(url.into()).await;
    let mut second = Box::pin(Submitting::start(url.into()));
    assert!((&mut second).now_or_never().is_none());
    // Another PR's submission does not wait.
    let other = Submitting::start(format!("{url}0")).now_or_never();
    assert!(other.is_some());
    drop(other);
    drop(first);
    let second = second.await;
    drop(second);
    let submitting = SUBMITTING.lock().unwrap();
    assert!(!submitting.contains_key(url));
    assert!(!submitting.contains_key(&format!("{url}0")));
}

#[tokio::test]
async fn a_verdict_is_refused_to_the_account_that_opened_the_pr() {
    use crate::test_support::Server;
    use shoal_forge_types::backends::ForgeKind;

    let server = Server::start(|request| {
        let body = match request.line.split_whitespace().nth(1) {
            Some("/api/v1/user") => serde_json::json!({"login": "Agent"}),
            Some("/api/v1/repos/team/repo/pulls/7") => serde_json::json!({
                "number": 7, "title": "Fix", "state": "open", "user": {"login": "agent"},
                "head": {"ref": "topic", "sha": "a".repeat(40)}, "base": {"ref": "main"},
            }),
            _ => serde_json::json!({
                "number": 8, "title": "Fix", "state": "open", "user": {"login": "someone"},
                "head": {"ref": "topic", "sha": "b".repeat(40)}, "base": {"ref": "main"},
            }),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let repo = crate::test_support::repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = crate::test_support::agent_on(&server, "agent");
    let own = repo.verdict_head(&account, PullNumber::new(7));
    let error = own.await.unwrap_err();
    assert_eq!(error.downcast_ref::<OwnPull>().unwrap().login, "Agent");
    let other = repo.verdict_head(&account, PullNumber::new(8));
    assert_eq!(other.await.unwrap().unwrap().as_str(), "b".repeat(40));

    // An account without a login for the host is refused before any request.
    let received = server.received().len();
    let anonymous = Account::default();
    let missing = repo.verdict_head(&anonymous, PullNumber::new(8));
    assert!(missing.await.is_err());
    assert_eq!(server.received().len(), received);
}
