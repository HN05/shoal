//! Bitbucket Cloud: its REST shapes, and the credentials Shoal sends to it.
//! Bitbucket has no official CLI to own a login, so a user-configured
//! command supplies a token for each request; Shoal stores none.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    backends::rest::RestCheck,
    items::{
        action::{Edit, MergeMethod},
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
    },
    repo::locator::ItemRoute,
    review::{
        status::Verdict,
        thread::{Target, Thread},
        view::{Comment, Details, PrDetails, Review, ReviewComment},
    },
    transport::api::{HttpMethod, Request},
};

use super::Backend;

#[derive(Clone, Copy)]
pub(crate) struct Bitbucket;

impl Backend for Bitbucket {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::PullRequests
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repositories/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pullrequests"
    }

    fn number_field(&self) -> &'static str {
        "id"
    }

    fn commit_field(&self) -> &'static str {
        "hash"
    }

    fn login_hint(&self) -> &'static str {
        "Bitbucket API request failed; private repositories need [bitbucket] token_command"
    }
}

/// The `[bitbucket]` global config: how to sign in to Bitbucket Cloud.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Credentials {
    /// Argument array whose standard output is an access token, or an
    /// Atlassian API token when `email` is set.
    pub token_command: Vec<String>,
    /// The Atlassian account email an API token belongs to.
    pub email: Option<String>,
}

impl Credentials {
    pub fn validate(&self) -> Result<()> {
        if let Some(program) = self.token_command.first() {
            ensure!(
                !program.is_empty(),
                "bitbucket.token_command must name a program"
            );
        }
        ensure!(
            self.token_command.iter().all(|word| !word.contains('\0')),
            "bitbucket.token_command contains NUL"
        );
        if let Some(email) = &self.email {
            ensure!(
                !self.token_command.is_empty(),
                "bitbucket.email needs bitbucket.token_command"
            );
            ensure!(
                email.contains('@') && email.chars().all(|c| c.is_ascii_graphic()),
                "bitbucket.email must be an email address"
            );
        }
        Ok(())
    }

    /// The curl config line that signs a request in, or none without a
    /// configured token, which reaches only public repositories.
    pub(crate) async fn authorization(&self) -> Result<Option<Vec<u8>>> {
        let Some((program, args)) = self.token_command.split_first() else {
            return Ok(None);
        };
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        let token = shoal_subprocess::Run::new(command)
            .timeout(crate::transport::api::TIMEOUT)
            .output()
            .await
            .context("run bitbucket.token_command")?;
        Ok(Some(authorization(token.trim(), self.email.as_deref())?))
    }
}

/// An access token is a bearer token; an API token signs in with its email.
fn authorization(token: &str, email: Option<&str>) -> Result<Vec<u8>> {
    // The value goes into a curl config line; anything but a plain token is refused.
    ensure!(
        !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\')),
        "bitbucket.token_command printed no token or an unexpected token format"
    );
    let value = match email {
        Some(email) => format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
        None => format!("Bearer {token}"),
    };
    Ok(format!("header = \"Authorization: {value}\"\n").into_bytes())
}

impl ForgeRepo {
    /// An issue or pull request as Bitbucket reports it.
    pub(crate) fn bitbucket_current(
        &self,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        let state = match (kind, value["state"].as_str()) {
            (ItemKind::Pr, Some("OPEN")) => ItemState::Open,
            (ItemKind::Pr, Some("MERGED")) => ItemState::Merged,
            (ItemKind::Pr, Some("DECLINED" | "SUPERSEDED")) => ItemState::Closed,
            (ItemKind::Issue, Some("new" | "open" | "on hold")) => ItemState::Open,
            (
                ItemKind::Issue,
                Some("resolved" | "invalid" | "duplicate" | "wontfix" | "closed"),
            ) => ItemState::Closed,
            (_, state) => bail!("unrecognized Bitbucket state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: text(&value["source"]["branch"]["name"])
                    .context("pull request has no source branch")?,
                base: text(&value["destination"]["branch"]["name"])
                    .context("pull request has no destination branch")?,
                draft: value["draft"].as_bool().unwrap_or_default(),
            }),
        };
        let body = match kind {
            ItemKind::Issue => &value["content"]["raw"],
            ItemKind::Pr => &value["description"],
        };
        Ok(Current {
            item: Item {
                kind,
                number,
                url: self.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: Vec::new(),
                pr,
            },
            node_id: None,
            head_repository: text(&value["source"]["repository"]["full_name"]),
            body: text(body).unwrap_or_default(),
            head_sha: text(&value["source"]["commit"]["hash"]),
            base_sha: None,
            // Bitbucket does not report whether a pull request conflicts.
            conflict: None,
            reviewers: value["reviewers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|user| text(&user["account_id"]))
                .collect(),
            user_ids: Default::default(),
        })
    }
}

impl ForgeRepo {
    /// The newest status of each build reported on the pull request.
    pub(crate) async fn bitbucket_checks(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<RestCheck>> {
        let endpoint = format!("{}/statuses", self.item_endpoint(ItemKind::Pr, number));
        statuses(&self.pages(account, &endpoint).await?)
    }

    /// Each participant's approval or change request.
    pub(crate) async fn bitbucket_verdicts(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let pull = self
            .send(
                account,
                &Request::get(self.item_endpoint(ItemKind::Pr, number)),
            )
            .await?;
        verdicts(&pull)
    }

    pub(crate) async fn bitbucket_details(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Details> {
        let value = self
            .send(account, &Request::get(self.item_endpoint(kind, number)))
            .await?;
        let current = self.current(kind, number, &value)?;
        let author = match kind {
            ItemKind::Issue => &value["reporter"],
            ItemKind::Pr => &value["author"],
        };
        Ok(Details {
            number,
            title: current.item.title,
            state: current.item.state.as_str().into(),
            author: nickname(author),
            created_at: text(&value["created_on"]).unwrap_or_default(),
            labels: Vec::new(),
            body: current.body,
            pr: current
                .item
                .pr
                .map(|pr| PrDetails::new(pr.head, pr.base, pr.draft)),
            comments: None,
            reviews: None,
        })
    }

    /// Fills the item's comments and returns a pull request's reviews: one
    /// per inline thread, and one per approval or change request.
    pub(crate) async fn bitbucket_discussion(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        let endpoint = format!("{}/comments", self.item_endpoint(kind, number));
        let (comments, mut reviews) = threads(&self.pages(account, &endpoint).await?);
        details.comments = Some(comments);
        if kind == ItemKind::Issue {
            return Ok(None);
        }
        let verdicts = self.bitbucket_verdicts(account, number).await?;
        reviews.extend(verdicts.into_iter().map(|(author, verdict)| {
            Review {
                author,
                state: match verdict {
                    Verdict::Approved => "approved",
                    Verdict::ChangesRequested => "changes_requested",
                    Verdict::Commented => "commented",
                }
                .into(),
                submitted_at: String::new(),
                body: String::new(),
                comments: Vec::new(),
            }
        }));
        Ok(Some(reviews))
    }

    /// One update of the changed fields. Bitbucket has no labels, keeps
    /// draft state as a flag, and replaces reviewers by Atlassian account ID.
    pub(crate) fn bitbucket_edit(&self, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        ensure!(
            edit.add_labels.is_empty() && edit.remove_labels.is_empty(),
            "Bitbucket has no labels"
        );
        let item = &current.item;
        // A pull request update needs its title even when it keeps it.
        let mut fields = json!({ "title": edit.title.as_deref().unwrap_or(&item.title) });
        if let Some(body) = &edit.body {
            match item.kind {
                ItemKind::Issue => fields["content"] = json!({ "raw": body }),
                ItemKind::Pr => fields["description"] = body.clone().into(),
            }
        }
        if let Some(base) = &edit.base {
            fields["destination"] = json!({ "branch": { "name": base } });
        }
        if let Some(draft) = edit.draft {
            fields["draft"] = draft.into();
        }
        if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
            let mut reviewers: Vec<&String> = current
                .reviewers
                .iter()
                .filter(|id| !edit.remove_reviewers.contains(id))
                .collect();
            for id in &edit.add_reviewers {
                if !reviewers.contains(&id) {
                    reviewers.push(id);
                }
            }
            fields["reviewers"] = reviewers
                .into_iter()
                .map(|id| json!({ "account_id": id }))
                .collect();
        }
        Ok(vec![Request::new(
            HttpMethod::Put,
            self.item_endpoint(item.kind, item.number),
            Some(fields),
        )])
    }

    /// Decline a pull request, or resolve or reopen an issue. Bitbucket
    /// cannot reopen a declined pull request.
    pub(crate) fn bitbucket_state(&self, item: &Item, state: ItemState) -> Result<Request> {
        let endpoint = self.item_endpoint(item.kind, item.number);
        Ok(match (item.kind, state) {
            (ItemKind::Pr, ItemState::Open) => {
                bail!("Bitbucket cannot reopen a declined pull request")
            }
            (ItemKind::Pr, _) => {
                Request::new(HttpMethod::Post, format!("{endpoint}/decline"), None)
            }
            (ItemKind::Issue, ItemState::Open) => {
                Request::new(HttpMethod::Put, endpoint, Some(json!({ "state": "open" })))
            }
            (ItemKind::Issue, _) => Request::new(
                HttpMethod::Put,
                endpoint,
                Some(json!({ "state": "resolved" })),
            ),
        })
    }
}

/// Bitbucket's strategy for each merge method; rebasing fast-forwards.
pub(crate) fn merge(endpoint: String, method: MergeMethod, delete_branch: bool) -> Request {
    let strategy = match method {
        MergeMethod::Merge => "merge_commit",
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase_fast_forward",
    };
    Request::new(
        HttpMethod::Post,
        endpoint,
        Some(json!({ "merge_strategy": strategy, "close_source_branch": delete_branch })),
    )
}

/// The newest status of each build, by its key.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    let mut latest = BTreeMap::new();
    for status in statuses {
        let key = status["key"]
            .as_str()
            .context("Bitbucket status has no key")?;
        let name = status["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(key);
        let updated = status["updated_on"].as_str().unwrap_or_default();
        let state = match status["state"].as_str() {
            Some("INPROGRESS") => "Pending",
            Some("SUCCESSFUL") => "Success",
            Some("FAILED") => "Failure",
            Some("STOPPED") => "Cancelled",
            state => bail!("unknown Bitbucket build state {state:?}"),
        };
        if latest
            .get(key)
            .is_none_or(|(newest, _, _): &(&str, &str, &str)| *newest < updated)
        {
            latest.insert(key, (updated, name, state));
        }
    }
    Ok(latest
        .into_values()
        .map(|(_, name, state)| RestCheck {
            name: name.into(),
            state,
        })
        .collect())
}

/// Participants who approved or requested changes.
fn verdicts(pull: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for participant in pull["participants"]
        .as_array()
        .context("pull request has no participants")?
    {
        let verdict = match participant["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("changes_requested") => Verdict::ChangesRequested,
            None if participant["approved"] == true => Verdict::Approved,
            None => continue,
            Some(state) => bail!("unknown Bitbucket participant state {state}"),
        };
        verdicts.push((nickname(&participant["user"]), verdict));
    }
    Ok(verdicts)
}

/// Comments, and a review for each inline thread with its replies.
fn threads(comments: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let comments = listed(comments);
    let root = roots(&comments);
    let mut general = Vec::new();
    let mut reviews: Vec<(u64, Review)> = Vec::new();
    for comment in &comments {
        let Some(id) = comment["id"].as_u64() else {
            continue;
        };
        let thread = root(id);
        let start = comments.iter().find(|candidate| candidate["id"] == thread);
        if !start.is_some_and(|start| start["inline"].is_object()) {
            general.push(Comment {
                author: nickname(&comment["user"]),
                created_at: text(&comment["created_on"]).unwrap_or_default(),
                body: text(&comment["content"]["raw"]).unwrap_or_default(),
            });
            continue;
        }
        // Bitbucket records resolution on the thread's first comment.
        let resolved = start.is_some_and(|start| start["resolution"].is_object());
        let review_comment = review_comment(comment, thread, resolved);
        match reviews.iter_mut().find(|(root, _)| *root == thread) {
            Some((_, review)) => review.comments.push(review_comment),
            None => reviews.push((
                thread,
                Review {
                    author: review_comment.author.clone(),
                    state: "commented".into(),
                    submitted_at: review_comment.created_at.clone(),
                    body: String::new(),
                    comments: vec![review_comment],
                },
            )),
        }
    }
    (
        general,
        reviews.into_iter().map(|(_, review)| review).collect(),
    )
}

/// The thread with `comment`, named by its first comment.
pub(crate) fn bitbucket_thread(comments: &[Value], comment: u64) -> Option<Thread> {
    let comments = listed(comments);
    let root = roots(&comments);
    comments.iter().find(|value| value["id"] == comment)?;
    let first = root(comment);
    let start = comments.iter().find(|value| value["id"] == first)?;
    let replies = comments
        .iter()
        .filter_map(|value| value["id"].as_u64())
        .filter(|id| *id != first && root(*id) == first)
        .count();
    Some(Thread {
        first,
        replies,
        resolved: Some(start["resolution"].is_object()),
        target: Target::Bitbucket,
    })
}

fn listed(comments: &[Value]) -> Vec<&Value> {
    comments
        .iter()
        .filter(|comment| comment["deleted"] != true)
        .collect()
}

/// The first comment of the thread of each comment ID.
fn roots(comments: &[&Value]) -> impl Fn(u64) -> u64 {
    let parents: BTreeMap<u64, u64> = comments
        .iter()
        .filter_map(|comment| Some((comment["id"].as_u64()?, comment["parent"]["id"].as_u64()?)))
        .collect();
    move |mut id| {
        while let Some(parent) = parents.get(&id) {
            id = *parent;
        }
        id
    }
}

/// An inline comment on the new side, or the old side of a removed line.
fn review_comment(comment: &Value, thread: u64, resolved: bool) -> ReviewComment {
    let inline = &comment["inline"];
    ReviewComment {
        id: comment["id"].as_u64().unwrap_or_default(),
        thread,
        author: nickname(&comment["user"]),
        created_at: text(&comment["created_on"]).unwrap_or_default(),
        path: text(&inline["path"]).unwrap_or_default(),
        line: inline["to"].as_u64().or(inline["from"].as_u64()),
        body: text(&comment["content"]["raw"]).unwrap_or_default(),
        resolved: Some(resolved),
    }
}

/// A deleted account has no user.
pub(crate) fn nickname(user: &Value) -> String {
    text(&user["nickname"])
        .or_else(|| text(&user["display_name"]))
        .unwrap_or_else(|| "ghost".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bitbucket() -> ForgeRepo {
        ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap()
    }

    #[test]
    fn tokens_are_bearer_or_basic_with_an_email() {
        assert_eq!(
            authorization("abc", None).unwrap(),
            b"header = \"Authorization: Bearer abc\"\n"
        );
        assert_eq!(
            authorization("abc", Some("me@example.com")).unwrap(),
            format!(
                "header = \"Authorization: Basic {}\"\n",
                BASE64.encode("me@example.com:abc")
            )
            .into_bytes()
        );
        for token in ["", "a b", "a\"b", "a\\b", "a\nb"] {
            assert!(authorization(token, None).is_err(), "{token:?}");
        }
    }

    #[test]
    fn credentials_need_a_program_for_an_email() {
        let email = Credentials {
            email: Some("me@example.com".into()),
            ..Credentials::default()
        };
        assert!(email.validate().is_err());
        let complete = Credentials {
            token_command: vec!["security".into(), "find-generic-password".into()],
            ..email
        };
        assert!(complete.validate().is_ok());
        let blank = Credentials {
            token_command: vec![String::new()],
            email: None,
        };
        assert!(blank.validate().is_err());
    }

    #[test]
    fn pull_requests_read_branches_drafts_and_reviewers() {
        let pull = json!({"id": 7, "title": "Fix", "state": "DECLINED", "description": "Why",
            "draft": true, "reviewers": [{"account_id": "557058:abc", "nickname": "kim"}],
            "source": {"branch": {"name": "topic"}, "commit": {"hash": "abc123"},
                "repository": {"full_name": "workspace/repo"}},
            "destination": {"branch": {"name": "main"}}});
        let current = bitbucket().current(ItemKind::Pr, 7, &pull).unwrap();
        assert_eq!(
            current.item,
            Item {
                kind: ItemKind::Pr,
                number: 7,
                url: "https://bitbucket.org/workspace/repo/pull-requests/7".into(),
                title: "Fix".into(),
                state: ItemState::Closed,
                labels: Vec::new(),
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: true,
                }),
            }
        );
        assert_eq!(current.reviewers, ["557058:abc"]);
        assert_eq!(current.head_repository.as_deref(), Some("workspace/repo"));
        assert_eq!(current.body, "Why");
        assert!(bitbucket().current(ItemKind::Pr, 8, &pull).is_err());
    }

    #[test]
    fn statuses_keep_the_newest_of_each_key() {
        let listed = json!([
            {"key": "ci", "name": "Pipeline", "state": "FAILED", "updated_on": "2026-01-01T00:00:00Z"},
            {"key": "ci", "name": "Pipeline", "state": "INPROGRESS", "updated_on": "2026-01-02T00:00:00Z"},
            {"key": "deploy", "name": "", "state": "STOPPED", "updated_on": "2026-01-01T00:00:00Z"}
        ]);
        let checks = statuses(listed.as_array().unwrap()).unwrap();
        assert_eq!(
            checks
                .iter()
                .map(|check| (check.name.as_str(), check.state))
                .collect::<Vec<_>>(),
            [("Pipeline", "Pending"), ("deploy", "Cancelled")]
        );
        assert!(statuses(&[json!({"key": "x", "state": "LATER"})]).is_err());
    }

    #[test]
    fn participants_approve_or_request_changes() {
        let pull = json!({"participants": [
            {"user": {"nickname": "ann"}, "role": "REVIEWER", "approved": true, "state": "approved"},
            {"user": {"nickname": "bob"}, "role": "REVIEWER", "approved": false, "state": "changes_requested"},
            {"user": {"display_name": "Cat"}, "role": "PARTICIPANT", "approved": false, "state": null},
            {"user": {"nickname": "dan"}, "role": "REVIEWER", "approved": true}
        ]});
        assert_eq!(
            verdicts(&pull).unwrap(),
            [
                ("ann".to_owned(), Verdict::Approved),
                ("bob".to_owned(), Verdict::ChangesRequested),
                ("dan".to_owned(), Verdict::Approved)
            ]
        );
        assert!(verdicts(&json!({})).is_err());
    }

    #[test]
    fn inline_threads_become_reviews_with_their_resolution() {
        let listed = json!([
            {"id": 1, "user": {"nickname": "ann"}, "content": {"raw": "Looks fine"}, "created_on": "t1"},
            {"id": 2, "user": {"nickname": "bob"}, "content": {"raw": "Bug"}, "created_on": "t2",
                "inline": {"path": "a.rs", "to": 3, "from": null}, "resolution": {"type": "comment_resolution"}},
            {"id": 3, "user": {"nickname": "ann"}, "content": {"raw": "Fixed"}, "created_on": "t3",
                "parent": {"id": 2}, "inline": {"path": "a.rs", "to": 3}},
            {"id": 4, "user": {"nickname": "bob"}, "content": {"raw": "Agreed"}, "created_on": "t4",
                "parent": {"id": 1}},
            {"id": 5, "user": {"nickname": "bob"}, "content": {"raw": ""}, "deleted": true}
        ]);
        let (comments, reviews) = threads(listed.as_array().unwrap());
        assert_eq!(
            comments.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
            ["Looks fine", "Agreed"]
        );
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].author, "bob");
        let thread = &reviews[0].comments;
        assert_eq!(
            thread
                .iter()
                .map(|c| (c.id, c.thread, c.body.as_str(), c.line, c.resolved))
                .collect::<Vec<_>>(),
            [
                (2, 2, "Bug", Some(3), Some(true)),
                (3, 2, "Fixed", Some(3), Some(true))
            ]
        );
    }

    #[test]
    fn replies_find_the_thread_they_belong_to() {
        let listed = json!([
            {"id": 2, "resolution": {"type": "comment_resolution"}},
            {"id": 3, "parent": {"id": 2}},
            {"id": 4, "parent": {"id": 3}},
            {"id": 5, "parent": {"id": 2}, "deleted": true},
            {"id": 6}
        ]);
        let listed = listed.as_array().unwrap();
        let thread = bitbucket_thread(listed, 4).unwrap();
        assert_eq!(
            (thread.first, thread.replies, thread.resolved),
            (2, 2, Some(true))
        );
        assert_eq!(bitbucket_thread(listed, 6).unwrap().resolved, Some(false));
        assert!(bitbucket_thread(listed, 5).is_none());
    }

    #[test]
    fn changes_keep_the_title_and_cannot_reopen_pull_requests() {
        let mut current = bitbucket()
            .current(
                ItemKind::Pr,
                7,
                &json!({"id": 7, "title": "Fix", "state": "OPEN",
                    "reviewers": [{"account_id": "a"}, {"account_id": "b"}],
                    "source": {"branch": {"name": "topic"}}, "destination": {"branch": {"name": "main"}}}),
            )
            .unwrap();
        let edit = Edit {
            body: Some("Why".into()),
            base: Some("next".into()),
            draft: Some(true),
            add_reviewers: vec!["c".into()],
            remove_reviewers: vec!["a".into()],
            ..Edit::default()
        };
        let requests = bitbucket().bitbucket_edit(&current, &edit).unwrap();
        assert_eq!(
            requests,
            [Request::new(
                HttpMethod::Put,
                "repositories/workspace/repo/pullrequests/7".into(),
                Some(json!({"title": "Fix", "description": "Why", "draft": true,
                    "destination": {"branch": {"name": "next"}},
                    "reviewers": [{"account_id": "b"}, {"account_id": "c"}]})),
            )]
        );
        let labels = Edit {
            add_labels: vec!["bug".into()],
            ..Edit::default()
        };
        assert!(bitbucket().bitbucket_edit(&current, &labels).is_err());
        assert_eq!(
            bitbucket()
                .bitbucket_state(&current.item, ItemState::Closed)
                .unwrap()
                .endpoint,
            "repositories/workspace/repo/pullrequests/7/decline"
        );
        current.item.state = ItemState::Closed;
        assert!(
            bitbucket()
                .bitbucket_state(&current.item, ItemState::Open)
                .is_err()
        );
        current.item.kind = ItemKind::Issue;
        current.item.pr = None;
        assert_eq!(
            bitbucket()
                .bitbucket_state(&current.item, ItemState::Open)
                .unwrap()
                .body,
            Some(json!({"state": "open"}))
        );
        assert_eq!(
            merge("merge".into(), MergeMethod::Rebase, true).body,
            Some(json!({"merge_strategy": "rebase_fast_forward", "close_source_branch": true}))
        );
    }

    #[test]
    fn issue_states_fold_into_open_and_closed() {
        for (state, expected) in [
            ("new", ItemState::Open),
            ("on hold", ItemState::Open),
            ("wontfix", ItemState::Closed),
            ("resolved", ItemState::Closed),
        ] {
            let issue =
                json!({"id": 3, "title": "Bug", "state": state, "content": {"raw": "Text"}});
            let current = bitbucket().current(ItemKind::Issue, 3, &issue).unwrap();
            assert_eq!(current.item.state, expected);
            assert_eq!(current.body, "Text");
        }
        let issue = json!({"id": 3, "title": "Bug", "state": "OPEN"});
        assert!(bitbucket().current(ItemKind::Issue, 3, &issue).is_err());
    }
}
