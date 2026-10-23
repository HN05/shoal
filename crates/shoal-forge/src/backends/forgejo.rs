//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the token fj saved for the host, else through
//! tea's login, else anonymously.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub(crate) mod threads;

use super::{
    Backend,
    github_rest::{self, Dialect},
    rest, tea,
};
use crate::{
    ForgeRepo,
    account::{Account, ForgejoLogin},
    backends::rest::{RestCheck, RestReads, fingerprints},
    items::{
        action::{Edit, MergeMethod, draft_title},
        create::{NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    review::{
        status::{PrStatus, Verdict, forgejo_reviews},
        thread::{Target, Thread},
    },
    transport::api::{
        HttpMethod, MAX_RESPONSE_BYTES, Request, authorization, full_page, json_body,
    },
    updates::{DISCUSSION, Snapshot},
};

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy)]
pub(crate) struct Forgejo;

impl Backend for Forgejo {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repos/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pulls"
    }

    fn number_field(&self) -> &'static str {
        "number"
    }

    fn commit_field(&self) -> &'static str {
        "sha"
    }

    fn login_hint(&self) -> &'static str {
        "Forgejo API request failed; private repositories need an fj or tea login"
    }

    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value> {
        let url = format!(
            "{}://{}/api/v1/{}",
            repo.web_scheme, repo.host, request.endpoint
        );
        match account.forgejo_login(&repo.host).await? {
            ForgejoLogin::Tea(login) => {
                json_body(&tea::send(account, &login, request, MAX_RESPONSE_BYTES).await?)
            }
            ForgejoLogin::Token(token) => {
                repo.curl_send(&url, Some(authorization(token)), request)
                    .await
            }
            ForgejoLogin::Anonymous => repo.curl_send(&url, None, request).await,
        }
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("limit", PAGE_SIZE)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }

    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        github_rest::current(repo, kind, number, value, Dialect::Forgejo)
    }

    fn numbers_items_together(&self) -> bool {
        true
    }

    async fn open_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Vec<list::Item>> {
        let endpoint = match kind {
            ItemKind::Issue => "issues?state=open&type=issues",
            ItemKind::Pr => "pulls?state=open",
        };
        rest::open_items(repo, account, endpoint).await
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        Ok(github_rest::edit(repo, current, edit, Dialect::Forgejo))
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        github_rest::comment(repo, item, body)
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        Ok(github_rest::set_state(repo, item, state))
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let number = current.item.number;
        Ok(vec![Request::new(
            HttpMethod::Post,
            format!("{}/merge", repo.item_endpoint(ItemKind::Pr, number)),
            Some(json!({
                "Do": method.as_str(),
                "delete_branch_after_merge": delete_branch,
            })),
        )])
    }

    fn comment_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "id")
    }

    /// Forgejo counts a reply in its review.
    fn reply_key(&self) -> (&'static str, &'static str) {
        ("reviews", "pull_request_review_id")
    }

    /// Forgejo cannot filter its list by head branch.
    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _branch: &str,
    ) -> Result<Vec<Value>> {
        repo.pages(account, &repo.repo_endpoint("pulls?state=open"))
            .await
    }

    fn has_head(&self, repo: &ForgeRepo, pull: &Value, branch: &str) -> bool {
        github_rest::has_head(repo, pull, branch)
    }

    /// Forgejo keeps draft state in the title.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let title = if pull.draft {
            draft_title(&pull.title, true)
        } else {
            pull.title.clone()
        };
        let body = json!({"title": title, "body": pull.body, "head": pull.head, "base": pull.base});
        Request::new(HttpMethod::Post, repo.repo_endpoint("pulls"), Some(body))
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "body": issue.body });
        Request::new(HttpMethod::Post, repo.repo_endpoint("issues"), Some(body))
    }

    /// Forgejo takes only label IDs when opening an issue; the label set
    /// takes names.
    fn labels_issue_after_opening(&self) -> bool {
        true
    }

    fn has_labels(&self) -> bool {
        true
    }

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>> {
        let reviews = threads::review_comments(repo, account, number).await?;
        Ok(threads::thread(&reviews, comment))
    }

    fn resolves_threads(&self) -> bool {
        false
    }

    /// A reply is a comment of the thread's review on the first comment's
    /// lines.
    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        let Target::Forgejo {
            review,
            path,
            line,
            extra_lines,
        } = &thread.target
        else {
            bail!("not a Forgejo review thread");
        };
        let side = if *line < 0 {
            "old_position"
        } else {
            "new_position"
        };
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("pulls/{number}/reviews/{review}/comments")),
            Some(json!({ "body": body, "path": path, side: line.abs(),
                "extra_lines_count": extra_lines })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: u64,
        _thread: &Thread,
        _resolved: bool,
    ) -> Result<Request> {
        bail!("Forgejo review threads cannot be resolved through its API")
    }

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot> {
        rest::issue_activity(self, repo, account, number).await
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        rest::pr_activity(self, repo, account, number, branch).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        rest::pr_status(self, repo, account, number, url).await
    }
}

impl RestReads for Forgejo {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
        let comments = repo.pages(account, &endpoint).await?;
        fingerprints(&comments, &["body", "updated_at"])
    }

    async fn review_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        let reviews = repo.pages(account, &endpoint).await?;
        fingerprints(
            &reviews,
            &[
                "body",
                "state",
                "comments_count",
                "stale",
                "dismissed",
                "updated_at",
            ],
        )
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull.head_sha.as_deref().context("PR has no head commit")?;
        let endpoint = repo.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        let combined = repo.send(account, &Request::get(endpoint)).await?;
        statuses(&combined)
    }

    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        forgejo_reviews(&repo.pages(account, &endpoint).await?)
    }
}

/// Forgejo's combined status; a commit without statuses has `"statuses": null`.
fn statuses(combined: &Value) -> Result<Vec<RestCheck>> {
    let statuses = match &combined["statuses"] {
        Value::Null => &[][..],
        statuses => statuses.as_array().context("invalid Forgejo CI statuses")?,
    };
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "Forgejo returned a partial page of CI statuses"
    );
    statuses
        .iter()
        .map(|status| {
            let name = status["context"]
                .as_str()
                .context("Forgejo CI status has no context")?;
            let state = match status["status"].as_str() {
                Some("pending") => "Pending",
                Some("success") => "Success",
                Some("failure") => "Failure",
                Some("warning") => "Warning",
                Some("skipped") => "Skipped",
                Some("error") => "Error",
                _ => anyhow::bail!("unknown Forgejo CI state"),
            };
            Ok(RestCheck {
                name: name.into(),
                state,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgejo_statuses_need_every_status_and_known_states() {
        let combined = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"},
            {"context": "review", "status": "pending"}
        ]});
        assert_eq!(
            statuses(&combined).unwrap(),
            [
                RestCheck {
                    name: "ci / rust".into(),
                    state: "Success"
                },
                RestCheck {
                    name: "review".into(),
                    state: "Pending"
                },
            ]
        );
        assert!(
            statuses(&json!({"total_count": 0, "statuses": null}))
                .unwrap()
                .is_empty()
        );
        let partial = json!({"total_count": 3, "statuses": combined["statuses"]});
        assert!(statuses(&partial).is_err());
        let unknown = json!({"total_count": 1, "statuses": [{"context": "x", "status": "later"}]});
        assert!(statuses(&unknown).is_err());
    }

    #[test]
    fn pages_end_at_a_short_page() {
        let full = serde_json::json!([1, 2]);
        assert!(Forgejo.page(&full, 2).unwrap().1);
        assert!(!Forgejo.page(&full, 3).unwrap().1);
    }
}
