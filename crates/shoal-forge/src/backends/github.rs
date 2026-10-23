//! GitHub, through `gh`, which owns the login: its CLI reports items and
//! activity as structured data, and `gh api` sends REST and GraphQL requests.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

mod activity;
pub(crate) mod threads;
mod view;

use super::{
    Backend,
    github_rest::{self, Dialect},
};
use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    items::{
        action::{Edit, MergeMethod},
        create::{NewIssue, NewPull},
        issue::require_open,
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    review::{
        status::PrStatus,
        thread::{Target, Thread},
        view::{Details, Review},
    },
    transport::{
        api::{HttpMethod, Request, encode, full_page},
        programs,
    },
    updates::{DISCUSSION, Snapshot},
};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

impl Backend for GitHub {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
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
        "check `gh auth login` and repository access"
    }

    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value> {
        let body = repo.passthrough(programs::GH, account, request).await?;
        // GraphQL reports a failed mutation in a successful response.
        if let Some(errors) = graphql_errors(&body) {
            bail!("{errors}");
        }
        Ok(body)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
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
        github_rest::current(repo, kind, number, value, Dialect::GitHub)
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
        let noun = match kind {
            ItemKind::Issue => "issue",
            ItemKind::Pr => "pr",
        };
        let repo = format!("{}/{}", repo.host, repo.path);
        let args = [
            noun,
            "list",
            "--repo",
            &repo,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title",
        ];
        let text = gh(account, &args, Query::List).await?;
        serde_json::from_str(&text).with_context(|| format!("invalid gh {noun} list response"))
    }

    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<ItemState> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let args = [
            "issue",
            "view",
            &id,
            "--repo",
            &repo,
            "--json",
            "number,state",
        ];
        let output = gh(account, &args, Query::Issue).await?;
        reported_state(&output, number)
    }

    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let args = [
            "issue",
            "view",
            &id,
            "--repo",
            &repo,
            "--json",
            "number,title,body,state",
        ];
        let text = gh(account, &args, Query::Issue).await?;
        require_open(reported_state(&text, number)?)?;
        #[derive(serde::Deserialize)]
        struct GitHubIssue {
            number: u64,
            title: String,
            body: Option<String>,
        }
        let issue: GitHubIssue =
            serde_json::from_str(&text).context("invalid gh issue response")?;
        ensure!(issue.number == number, "gh returned a different issue");
        Ok((issue.title, issue.body.unwrap_or_default()))
    }

    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let fields = "number,title,headRefName,baseRefName,isCrossRepository";
        let args = ["pr", "view", &id, "--repo", &repo, "--json", fields];
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Pull {
            number: u64,
            title: String,
            head_ref_name: String,
            base_ref_name: String,
            is_cross_repository: bool,
        }
        let output = gh(account, &args, Query::Pull("")).await?;
        let pull: Pull = serde_json::from_str(&output).context("invalid gh PR response")?;
        ensure!(pull.number == number, "gh returned a different PR");
        ensure!(
            !pull.is_cross_repository,
            "PR #{number} comes from a fork; only branches in this repository can be opened"
        );
        Ok((pull.title, pull.head_ref_name, pull.base_ref_name))
    }

    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        let number = number.to_string();
        let output = gh(
            account,
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &format!("{}/{}", repo.host, repo.path),
                "--json",
                "number,state,headRefName,commits",
            ],
            Query::Pull(MERGED_HINT),
        )
        .await?;
        #[derive(serde::Deserialize)]
        struct Commit {
            oid: String,
        }
        #[derive(serde::Deserialize)]
        struct Pull {
            number: u64,
            state: String,
            #[serde(rename = "headRefName")]
            branch: String,
            commits: Vec<Commit>,
        }
        let pull: Pull = serde_json::from_str(&output)?;
        ensure!(
            pull.number.to_string() == number && pull.branch == branch,
            "PR does not match the workspace branch"
        );
        ensure!(
            matches!(pull.state.as_str(), "OPEN" | "CLOSED" | "MERGED"),
            "unknown PR state"
        );
        Ok((pull.state == "MERGED").then(|| pull.commits.into_iter().map(|c| c.oid).collect()))
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let mut requests = github_rest::edit(repo, current, edit, Dialect::GitHub);
        if let Some(draft) = github_rest::draft_change(current, edit) {
            requests.push(draft_request(current, draft)?);
        }
        Ok(requests)
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
        let item = &current.item;
        let pr = item.pr.as_ref().context("only PRs merge")?;
        let merge = format!("{}/merge", repo.item_endpoint(ItemKind::Pr, item.number));
        // GitHub can answer success without merging.
        let mut requests = vec![
            Request::new(
                HttpMethod::Put,
                merge,
                Some(json!({ "merge_method": method.as_str() })),
            )
            .confirming("merged"),
        ];
        // A fork's branch is not this repository's to delete.
        if delete_branch && current.head_repository.as_deref() == Some(repo.path.as_str()) {
            requests.push(Request::new(
                HttpMethod::Delete,
                repo.repo_endpoint(&format!("git/refs/heads/{}", encode(&pr.head, true))),
                None,
            ));
        }
        Ok(requests)
    }

    /// GitHub reads discussion comments by GraphQL node ID, which REST
    /// returns as `node_id`.
    fn comment_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "node_id")
    }

    fn reply_key(&self) -> (&'static str, &'static str) {
        ("inline", "id")
    }

    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        branch: &str,
    ) -> Result<Vec<Value>> {
        let owner = repo.path.split('/').next().unwrap_or_default();
        let head = encode(&format!("{owner}:{branch}"), false);
        let endpoint = repo.repo_endpoint(&format!("pulls?state=open&head={head}"));
        repo.send(account, &Request::get(endpoint))
            .await?
            .as_array()
            .cloned()
            .context("the PR list is not a list")
    }

    fn has_head(&self, repo: &ForgeRepo, pull: &Value, branch: &str) -> bool {
        github_rest::has_head(repo, pull, branch)
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let body = json!({"title": pull.title, "body": pull.body, "head": pull.head,
            "base": pull.base, "draft": pull.draft});
        Request::new(HttpMethod::Post, repo.repo_endpoint("pulls"), Some(body))
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "body": issue.body, "labels": issue.labels });
        Request::new(HttpMethod::Post, repo.repo_endpoint("issues"), Some(body))
    }

    fn labels_issue_after_opening(&self) -> bool {
        false
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
        Ok(threads::thread(
            &threads::threads(repo, account, number).await?,
            comment,
        ))
    }

    fn resolves_threads(&self) -> bool {
        true
    }

    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        let endpoint =
            repo.repo_endpoint(&format!("pulls/{number}/comments/{}/replies", thread.first));
        Ok(Request::new(
            HttpMethod::Post,
            endpoint,
            Some(json!({ "body": body })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: u64,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let Target::GitHub { node } = &thread.target else {
            bail!("not a GitHub review thread");
        };
        let mutation = if resolved {
            "resolveReviewThread"
        } else {
            "unresolveReviewThread"
        };
        Ok(Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({
                "query": format!(
                    "mutation($id: ID!) {{ {mutation}(input: {{threadId: $id}}) {{ thread {{ isResolved }} }} }}"
                ),
                "variables": { "id": node },
            })),
        ))
    }

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot> {
        activity::issue_activity(repo, account, number)
            .await
            .context(self.login_hint())
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        activity::pr_activity(repo, account, number, branch).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        activity::pr_status(repo, account, number, url).await
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details> {
        view::details(repo, account, kind, number, comments).await
    }

    /// `gh` reads the comments with the item; this reads the reviews.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        view::reviews(repo, account, kind, number).await
    }
}

/// What a `gh` read is for, which decides its timeout and error guidance.
#[derive(Clone, Copy)]
pub(crate) enum Query {
    Issue,
    Pull(&'static str),
    List,
    /// Reading an issue or PR for `shoal view`.
    View,
}

/// Run `gh` as `account`, the CLI that reads GitHub's items.
pub(crate) async fn gh(account: &Account, args: &[&str], query: Query) -> Result<String> {
    let tool = programs::GH;
    let mut command = tokio::process::Command::new(tool);
    command.args(args).env("NO_COLOR", "1");
    account.apply(&mut command);
    let seconds = match query {
        Query::Issue | Query::List | Query::View => 30,
        Query::Pull(_) => 20,
    };
    let output = shoal_subprocess::Run::new(command)
        .timeout(std::time::Duration::from_secs(seconds))
        .capture()
        .await;
    query.response(tool, output)
}

impl Query {
    fn response(self, tool: &str, output: std::io::Result<std::process::Output>) -> Result<String> {
        match self {
            Self::Issue => {
                shoal_subprocess::checked_output(tool, output).with_context(|| {
                    format!("{tool} issue lookup failed; install it and check `{tool} auth login` and repository access before using --issue")
                })
            }
            Self::Pull(hint) => shoal_subprocess::checked_output(tool, output)
                .with_context(|| format!("PR lookup requires {tool} and its existing login{hint}")),
            Self::List => shoal_subprocess::checked_output(tool, output).with_context(|| {
                format!("{tool} search failed; install it and check `{tool} auth login` and repository access")
            }),
            Self::View => shoal_subprocess::checked_output(tool, output).with_context(|| {
                format!("{tool} lookup failed; install it and check `{tool} auth login` and repository access")
            }),
        }
    }
}

/// The state `gh issue view --json number,state` reports.
fn reported_state(output: &str, number: u64) -> Result<ItemState> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
    }
    #[derive(serde::Deserialize)]
    struct Issue {
        number: u64,
        state: State,
    }
    let issue: Issue = serde_json::from_str(output).context("invalid gh issue response")?;
    ensure!(issue.number == number, "gh returned a different issue");
    Ok(match issue.state {
        State::Open => ItemState::Open,
        State::Closed => ItemState::Closed,
    })
}

/// GitHub changes draft state only through GraphQL.
fn draft_request(current: &Current, draft: bool) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    let mutation = if draft {
        "convertPullRequestToDraft"
    } else {
        "markPullRequestReadyForReview"
    };
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({
            "query": format!(
                "mutation($id: ID!) {{ {mutation}(input: {{pullRequestId: $id}}) {{ clientMutationId }} }}"
            ),
            "variables": { "id": id },
        })),
    ))
}

/// The messages of a GraphQL response's `errors`, when it has any.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())?;
    Some(
        errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_errors_preserve_login_guidance_and_diagnostic_limits() {
        use std::{io, os::unix::process::ExitStatusExt, process::Output};

        for tool in ["gh", "fj"] {
            for (query, limit, hint) in [
                (
                    Query::Issue,
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "auth login",
                ),
                (
                    Query::Pull(MERGED_HINT),
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "shoal done",
                ),
                (
                    Query::Pull(""),
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "existing login",
                ),
            ] {
                let missing = query
                    .response(tool, Err(io::Error::from(io::ErrorKind::NotFound)))
                    .unwrap_err();
                let missing = format!("{missing:#}");
                assert!(missing.contains(tool), "{missing}");
                assert!(missing.contains(hint), "{missing}");

                let failed = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(256),
                            stdout: b"ignored".to_vec(),
                            stderr: format!("{}END", "é".repeat(limit)).into_bytes(),
                        }),
                    )
                    .unwrap_err();
                let failed = format!("{failed:#}");
                assert!(failed.contains(tool), "{failed}");
                assert!(failed.contains(hint), "{failed}");
                assert!(failed.ends_with(&"é".repeat(limit)));
                assert!(!failed.contains("END"));

                let invalid = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(0),
                            stdout: vec![0xff],
                            stderr: Vec::new(),
                        }),
                    )
                    .unwrap_err();
                assert!(format!("{invalid:#}").contains("output is not UTF-8"));
            }
        }
    }

    #[test]
    fn github_requires_matching_number_and_known_state() {
        for (state, expected) in [("OPEN", ItemState::Open), ("CLOSED", ItemState::Closed)] {
            let output = serde_json::json!({"number": 316, "state": state}).to_string();
            assert_eq!(reported_state(&output, 316).unwrap(), expected);
            assert!(reported_state(&output, 1).is_err());
        }
        for output in [
            r#"{"number":316,"state":"MERGED"}"#,
            r#"{"number":316}"#,
            "",
        ] {
            assert!(reported_state(output, 316).is_err());
        }
    }

    #[test]
    fn graphql_errors_fail_a_successful_response() {
        let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
        assert_eq!(
            graphql_errors(&failed).unwrap(),
            "not allowed; GraphQL error"
        );
        assert_eq!(
            graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
            None
        );
        assert_eq!(graphql_errors(&serde_json::json!([])), None);
    }
}
