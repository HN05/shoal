use super::*;

fn review(verdict: ReviewVerdict, body: &str, comments: &[(&str, u64, &str)]) -> NewReview {
    NewReview {
        verdict,
        body: body.into(),
        comments: comments
            .iter()
            .map(|&(path, line, body)| LineComment {
                path: path.into(),
                line,
                side: Side::New,
                body: body.into(),
            })
            .collect(),
    }
}

fn refusal(review: NewReview) -> String {
    review.validate().unwrap_err().to_string()
}

#[test]
fn reviews_need_a_body_or_line_comments_and_change_requests_a_body() {
    assert!(
        review(ReviewVerdict::Comment, "", &[("a.rs", 1, "x")])
            .validate()
            .is_ok()
    );
    assert!(
        review(ReviewVerdict::Approve, "Fine", &[])
            .validate()
            .is_ok()
    );
    assert_eq!(
        refusal(review(ReviewVerdict::Approve, " ", &[])),
        "a review needs a body, a line comment or both"
    );
    assert_eq!(
        refusal(review(
            ReviewVerdict::RequestChanges,
            "",
            &[("a.rs", 1, "x")]
        )),
        "a review that requests changes needs a body"
    );
    assert_eq!(
        refusal(review(ReviewVerdict::Comment, "", &[("a.rs", 0, "x")])),
        "a.rs:0: lines count from 1"
    );
    assert_eq!(
        refusal(review(ReviewVerdict::Comment, "", &[("a.rs", 3, "\n")])),
        "the line comment on a.rs:3 is empty"
    );
}

#[test]
fn line_comments_read_from_json_default_to_the_new_side_and_refuse_unknowns() {
    let comments: Vec<LineComment> = serde_json::from_str(
        r#"[{"path": "a.rs", "line": 4, "body": "x"},
            {"path": "b.rs", "line": 7, "side": "old", "body": "y"}]"#,
    )
    .unwrap();
    assert_eq!(comments[0].side, Side::New);
    assert_eq!(comments[1].to_string(), "b.rs:7 (old)");
    let unknown_field = r#"[{"path": "a.rs", "line": 4, "body": "x", "start_line": 2}]"#;
    assert!(serde_json::from_str::<Vec<LineComment>>(unknown_field).is_err());
    let unknown_side = r#"[{"path": "a.rs", "line": 4, "body": "x", "side": "left"}]"#;
    assert!(serde_json::from_str::<Vec<LineComment>>(unknown_side).is_err());
}

/// A submission of `drafts` and `requests`, each a POST to its endpoint.
fn submission(drafts: &[&str], requests: &[&str]) -> Submission {
    let post = |endpoint: &&str| {
        Request::new(
            crate::transport::api::HttpMethod::Post,
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
            crate::transport::api::HttpMethod::Delete,
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
