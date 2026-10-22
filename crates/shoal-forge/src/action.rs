//! Changes to an issue or PR, planned as the REST requests each forge needs
//! and applied in order, stopping at the first failure.
use std::borrow::Cow;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
    attach::{Attachment, Holder, Pending},
    bitbucket,
    gitlab::gitlab_merge,
    item::{Current, Item, ItemState, draft_prefix},
    link::ItemKind,
};
use crate::state::states;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Edit(Edit),
    Comment {
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    Close,
    Reopen,
    Merge {
        method: MergeMethod,
        delete_branch: bool,
    },
    Thread(ThreadChange),
}

/// Fields left empty stay as they are; base, reviewers and draft state are
/// PR-only. Attachments go into the new description, or after the current
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub add_reviewers: Vec<String>,
    pub remove_reviewers: Vec<String>,
    pub draft: Option<bool>,
    pub attachments: Vec<Attachment>,
}

/// A reply in the review thread of a PR's inline comment, a change to the
/// thread's resolution, or both, replying first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadChange {
    /// Any comment of the thread.
    pub comment: u64,
    pub reply: Option<String>,
    pub resolve: Option<bool>,
}

states!(MergeMethod: ValueEnum {
    Merge => "merge",
    Rebase => "rebase",
    Squash => "squash",
});

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Edit(_) => "edit",
            Self::Comment { .. } => "comment",
            Self::Close => "close",
            Self::Reopen => "reopen",
            Self::Merge { .. } => "merge",
            Self::Thread(change) => match change.resolve {
                Some(true) => "resolve",
                Some(false) => "unresolve",
                None => "reply",
            },
        }
    }

    /// The files the action attaches.
    pub fn attachments(&self) -> &[Attachment] {
        match self {
            Self::Edit(edit) => &edit.attachments,
            Self::Comment { attachments, .. } => attachments,
            _ => &[],
        }
    }

    /// Refuse what no forge can do with an item of `kind`.
    pub fn validate(&self, kind: ItemKind) -> Result<()> {
        let pr_only = |what: &str| -> Result<()> {
            ensure!(kind == ItemKind::Pr, "issues have no {what}");
            Ok(())
        };
        match self {
            Self::Edit(edit) => {
                ensure!(
                    *edit != Edit::default(),
                    "nothing to change; pass a new title, description, attachment, base, label, reviewer or draft state"
                );
                if edit.base.is_some() {
                    pr_only("base branch")?;
                }
                if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
                    pr_only("reviewers")?;
                }
                if edit.draft.is_some() {
                    pr_only("draft state")?;
                }
            }
            Self::Comment { body, attachments } => ensure!(
                !body.trim().is_empty() || !attachments.is_empty(),
                "the comment is empty"
            ),
            Self::Merge { .. } => pr_only("merge")?,
            Self::Thread(change) => {
                pr_only("review threads")?;
                ensure!(
                    change.reply.is_some() || change.resolve.is_some(),
                    "nothing to change; pass a reply or a resolution"
                );
                if let Some(reply) = &change.reply {
                    ensure!(!reply.trim().is_empty(), "the reply is empty");
                }
            }
            Self::Close | Self::Reopen => {}
        }
        Ok(())
    }
}

/// What an action left on the forge.
pub struct Acted {
    /// The item as the forge reports it afterwards.
    pub item: Item,
    /// The activity key of the comment or review reply the action posted.
    pub authored: Option<String>,
}

impl ForgeRepo {
    /// Apply `action` as `account` and return the item as the forge reports it
    /// afterwards.
    pub(crate) async fn act(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        action: &Action,
    ) -> Result<Item> {
        Ok(self.apply(account, kind, number, action).await?.item)
    }

    /// [`Self::act`], also naming the comment or reply the action posted.
    pub async fn apply(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        action: &Action,
    ) -> Result<Acted> {
        action.validate(kind)?;
        // Refused before anything is read or posted.
        let per_item = self.check_uploads(account, action.attachments()).await?;
        let current = self.read(account, kind, number).await?;
        let (action, pending) = self
            .upload_attachments(account, &current, action, per_item)
            .await?;
        let mut authored = None;
        let mut first = None;
        for request in self.requests(account, current, &action).await? {
            let response = self.send(account, &request).await?;
            // A comment, or a thread's reply, is the action's first request.
            if first.is_none() {
                authored = self.posted(&action, &response);
                first = Some(response);
            }
        }
        if let (Some(pending), Some(comment)) = (pending, first) {
            self.attach_to_comment(account, &comment, pending)
                .await
                .context("the comment was posted, but attaching its files failed")?;
        }
        Ok(Acted {
            item: self.read(account, kind, number).await?.item,
            authored,
        })
    }

    /// `action` with its attachments uploaded and embedded in its body, and
    /// the files of a comment that must exist before the forge takes them;
    /// `per_item` is whether the forge keeps them with an item or comment.
    async fn upload_attachments<'a>(
        &self,
        account: &Account,
        current: &Current,
        action: &'a Action,
        per_item: bool,
    ) -> Result<(Cow<'a, Action>, Option<Pending>)> {
        match action {
            Action::Edit(edit) if !edit.attachments.is_empty() => {
                let holder = if per_item {
                    Holder::Item(current.item.number)
                } else {
                    Holder::Repository
                };
                let body = edit.body.as_deref().unwrap_or(&current.body);
                let body = self
                    .attach(account, holder, body, &edit.attachments)
                    .await?;
                let edit = Edit {
                    body: Some(body),
                    attachments: Vec::new(),
                    ..edit.clone()
                };
                Ok((Cow::Owned(Action::Edit(edit)), None))
            }
            Action::Comment { body, attachments } if !attachments.is_empty() => {
                let (body, pending) = self.new_body(account, body, attachments, per_item).await?;
                let comment = Action::Comment {
                    body,
                    attachments: Vec::new(),
                };
                Ok((Cow::Owned(comment), pending))
            }
            _ => Ok((Cow::Borrowed(action), None)),
        }
    }

    /// Upload a posted comment's files to it and embed them in its body.
    async fn attach_to_comment(
        &self,
        account: &Account,
        comment: &serde_json::Value,
        pending: Pending,
    ) -> Result<()> {
        let id = comment_id(comment)?;
        let holder = Holder::Comment(id);
        let body = self
            .attach(account, holder, &pending.body, &pending.attachments)
            .await?;
        self.send(account, &self.comment_edit_request(id, &body))
            .await?;
        Ok(())
    }

    /// Replace the body of the discussion comment `id`, which only Forgejo,
    /// keeping files with comments, needs.
    fn comment_edit_request(&self, id: u64, body: &str) -> Request {
        let endpoint = self.repo_endpoint(&format!("issues/comments/{id}"));
        Request::new(HttpMethod::Patch, endpoint, Some(json!({ "body": body })))
    }

    /// The activity key of what `action` posted, from its response: the
    /// entry each forge's activity records it under. GitHub reads discussion
    /// comments by GraphQL node ID, which REST returns as `node_id`, and
    /// Forgejo counts a reply in its review. Without an ID the post is
    /// reported back like anyone else's, rather than failing a post that
    /// succeeded.
    fn posted(&self, action: &Action, response: &serde_json::Value) -> Option<String> {
        use super::updates::DISCUSSION;
        let reply = matches!(action, Action::Thread(change) if change.reply.is_some());
        let (source, field) = match (action, self.kind) {
            (Action::Comment { .. }, ForgeKind::GitHub) => (DISCUSSION, "node_id"),
            (Action::Comment { .. }, _) => (DISCUSSION, "id"),
            (_, ForgeKind::GitHub) if reply => ("inline", "id"),
            (_, ForgeKind::Forgejo) if reply => ("reviews", "pull_request_review_id"),
            (_, ForgeKind::GitLab | ForgeKind::Bitbucket) if reply => (DISCUSSION, "id"),
            _ => return None,
        };
        response
            .get(field)
            .filter(|id| id.is_string() || id.is_number())
            .map(|id| super::updates::activity_key(source, id))
    }

    /// The requests that carry out `action`, after reading what it needs
    /// beyond the item.
    async fn requests(
        &self,
        account: &Account,
        mut current: Current,
        action: &Action,
    ) -> Result<Vec<Request>> {
        match (action, self.kind) {
            (Action::Thread(change), _) => {
                let number = current.item.number;
                let thread = self.thread(account, number, change.comment).await?;
                self.plan_thread(number, &thread, change)
            }
            (Action::Edit(edit), ForgeKind::GitLab) => {
                self.gitlab_user_ids(account, &edit.add_reviewers, &mut current)
                    .await?;
                self.plan(&current, action)
            }
            _ => self.plan(&current, action),
        }
    }

    pub async fn read(&self, account: &Account, kind: ItemKind, number: u64) -> Result<Current> {
        let value = self
            .send(account, &Request::get(self.item_endpoint(kind, number)))
            .await?;
        self.current(kind, number, &value)
    }

    /// Whether `number` names an issue or a PR. Only forges that number both
    /// in one sequence can tell; elsewhere the caller names the kind.
    pub async fn item_kind(&self, account: &Account, number: u64) -> Result<ItemKind> {
        ensure!(
            matches!(self.kind, ForgeKind::GitHub | ForgeKind::Forgejo),
            "this forge numbers issues and PRs separately; put issue or pr before #{number}"
        );
        let value = self
            .send(
                account,
                &Request::get(self.item_endpoint(ItemKind::Issue, number)),
            )
            .await?;
        super::item::issue_read_kind(number, &value)
    }

    /// The requests that carry out `action` on `current`, in order.
    pub(crate) fn plan(&self, current: &Current, action: &Action) -> Result<Vec<Request>> {
        let item = &current.item;
        Ok(match action {
            Action::Edit(edit) => match self.kind {
                ForgeKind::GitHub | ForgeKind::Forgejo => self.plan_edit(current, edit)?,
                ForgeKind::GitLab => self.gitlab_edit(current, edit)?,
                ForgeKind::Bitbucket => self.bitbucket_edit(current, edit)?,
            },
            Action::Comment { body, .. } => vec![self.comment_request(item, body)],
            Action::Close => {
                ensure!(
                    item.state == ItemState::Open,
                    "#{} is not open",
                    item.number
                );
                vec![self.state_request(item, ItemState::Closed)?]
            }
            Action::Reopen => {
                ensure!(
                    item.state == ItemState::Closed,
                    "#{} is not closed, or was merged",
                    item.number
                );
                vec![self.state_request(item, ItemState::Open)?]
            }
            Action::Merge {
                method,
                delete_branch,
            } => {
                ensure!(
                    item.state == ItemState::Open,
                    "#{} is not open",
                    item.number
                );
                self.plan_merge(current, *method, *delete_branch)?
            }
            Action::Thread(_) => anyhow::bail!("a review thread change is planned from its thread"),
        })
    }

    /// Post `body` as a comment on the item.
    fn comment_request(&self, item: &Item, body: &str) -> Request {
        let item_endpoint = self.item_endpoint(item.kind, item.number);
        let (endpoint, body) = match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => (
                self.repo_endpoint(&format!("issues/{}/comments", item.number)),
                json!({ "body": body }),
            ),
            ForgeKind::GitLab => (format!("{item_endpoint}/notes"), json!({ "body": body })),
            ForgeKind::Bitbucket => (
                format!("{item_endpoint}/comments"),
                json!({ "content": { "raw": body } }),
            ),
        };
        Request::new(HttpMethod::Post, endpoint, Some(body))
    }

    /// Close or reopen the item.
    fn state_request(&self, item: &Item, state: ItemState) -> Result<Request> {
        let endpoint = self.item_endpoint(item.kind, item.number);
        let open = state == ItemState::Open;
        Ok(match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => Request::new(
                HttpMethod::Patch,
                endpoint,
                Some(json!({ "state": if open { "open" } else { "closed" } })),
            ),
            ForgeKind::GitLab => Request::new(
                HttpMethod::Put,
                endpoint,
                Some(json!({ "state_event": if open { "reopen" } else { "close" } })),
            ),
            ForgeKind::Bitbucket => self.bitbucket_state(item, state)?,
        })
    }

    fn plan_merge(
        &self,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let item = &current.item;
        let pr = item.pr.as_ref().context("only PRs merge")?;
        let merge = format!("{}/merge", self.item_endpoint(ItemKind::Pr, item.number));
        Ok(match self.kind {
            ForgeKind::GitHub => {
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
                if delete_branch && current.head_repository.as_deref() == Some(self.path.as_str()) {
                    requests.push(Request::new(
                        HttpMethod::Delete,
                        self.repo_endpoint(&format!(
                            "git/refs/heads/{}",
                            super::api::encode(&pr.head, true)
                        )),
                        None,
                    ));
                }
                requests
            }
            ForgeKind::Forgejo => vec![Request::new(
                HttpMethod::Post,
                merge,
                Some(json!({
                    "Do": method.as_str(),
                    "delete_branch_after_merge": delete_branch,
                })),
            )],
            ForgeKind::GitLab => vec![gitlab_merge(merge, method, delete_branch)?],
            ForgeKind::Bitbucket => vec![bitbucket::merge(merge, method, delete_branch)],
        })
    }

    fn plan_edit(&self, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let item = &current.item;
        let mut requests = Vec::new();
        let mut fields = serde_json::Map::new();
        let mut title = edit.title.clone();
        let draft = edit
            .draft
            .filter(|draft| item.pr.as_ref().is_some_and(|pr| pr.draft != *draft));
        // Forgejo keeps draft state in the title, so a new title carries it too.
        if let (Some(pr), ForgeKind::Forgejo) = (&item.pr, self.kind)
            && (title.is_some() || draft.is_some())
        {
            let current = title.as_deref().unwrap_or(&item.title);
            title = Some(draft_title(current, draft.unwrap_or(pr.draft)));
        }
        if let Some(title) = title {
            fields.insert("title".into(), title.into());
        }
        if let Some(body) = &edit.body {
            fields.insert("body".into(), body.clone().into());
        }
        if let Some(base) = &edit.base {
            fields.insert("base".into(), base.clone().into());
        }
        if !fields.is_empty() {
            requests.push(Request::new(
                HttpMethod::Patch,
                self.item_endpoint(item.kind, item.number),
                Some(fields.into()),
            ));
        }
        if !edit.add_labels.is_empty() || !edit.remove_labels.is_empty() {
            // Replacing the set takes label names on both forges.
            let mut labels: Vec<_> = item
                .labels
                .iter()
                .filter(|label| !edit.remove_labels.contains(label))
                .cloned()
                .collect();
            for label in &edit.add_labels {
                if !labels.contains(label) {
                    labels.push(label.clone());
                }
            }
            requests.push(Request::new(
                HttpMethod::Put,
                self.repo_endpoint(&format!("issues/{}/labels", item.number)),
                Some(json!({ "labels": labels })),
            ));
        }
        let reviewers = self.repo_endpoint(&format!("pulls/{}/requested_reviewers", item.number));
        for (method, reviewers_list) in [
            (HttpMethod::Post, &edit.add_reviewers),
            (HttpMethod::Delete, &edit.remove_reviewers),
        ] {
            if !reviewers_list.is_empty() {
                requests.push(Request::new(
                    method,
                    reviewers.clone(),
                    Some(json!({ "reviewers": reviewers_list })),
                ));
            }
        }
        if let (Some(draft), ForgeKind::GitHub) = (draft, self.kind) {
            requests.push(github_draft(current, draft)?);
        }
        Ok(requests)
    }
}

/// The ID of the comment a post's response describes.
fn comment_id(response: &serde_json::Value) -> Result<u64> {
    response["id"]
        .as_u64()
        .context("the forge did not report the comment's ID")
}

/// GitHub changes draft state only through GraphQL.
fn github_draft(current: &Current, draft: bool) -> Result<Request> {
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

/// The title with Forgejo's draft prefix added or removed.
pub(crate) fn draft_title(title: &str, draft: bool) -> String {
    let bare = draft_prefix(title).map_or(title, |len| title[len..].trim_start());
    if draft {
        format!("WIP: {bare}")
    } else {
        bare.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::PrSummary;
    use serde_json::Value;

    fn pr(forge: &ForgeRepo, draft: bool, labels: &[&str]) -> Current {
        Current {
            item: Item {
                kind: ItemKind::Pr,
                number: 7,
                url: String::new(),
                title: if draft && forge.kind == ForgeKind::Forgejo {
                    "WIP: Fix".into()
                } else {
                    "Fix".into()
                },
                state: ItemState::Open,
                labels: labels.iter().map(|label| label.to_string()).collect(),
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft,
                }),
            },
            node_id: Some("PR_1".into()),
            head_repository: Some("team/repo".into()),
            body: String::new(),
            head_sha: None,
            base_sha: None,
            conflict: None,
            reviewers: Vec::new(),
            user_ids: Default::default(),
        }
    }

    fn requests(
        forge: &ForgeRepo,
        current: &Current,
        action: Action,
    ) -> Vec<(String, String, Value)> {
        forge
            .plan(current, &action)
            .unwrap()
            .into_iter()
            .map(|request| {
                (
                    request.method.as_str().to_owned(),
                    request.endpoint,
                    request.body.unwrap_or_default(),
                )
            })
            .collect()
    }

    fn forges() -> [ForgeRepo; 2] {
        [
            ForgeRepo::parse("git@github.com:team/repo.git").unwrap(),
            ForgeRepo::parse("https://forge.example/team/repo").unwrap(),
        ]
    }

    #[test]
    fn posts_are_keyed_where_each_forge_records_their_activity() {
        let [github, forgejo] = forges();
        let gitlab = ForgeRepo::parse("https://gitlab.com/team/repo.git").unwrap();
        let comment = Action::Comment {
            body: "hi".into(),
            attachments: Vec::new(),
        };
        let reply = |reply: Option<&str>| {
            Action::Thread(ThreadChange {
                comment: 5,
                reply: reply.map(str::to_owned),
                resolve: Some(true),
            })
        };
        let response = json!({"id": 9, "node_id": "IC_9", "pull_request_review_id": 4});
        let posted = |forge: &ForgeRepo, action: &Action| forge.posted(action, &response);
        assert_eq!(
            posted(&github, &comment).as_deref(),
            Some("discussion:IC_9")
        );
        assert_eq!(posted(&forgejo, &comment).as_deref(), Some("discussion:9"));
        assert_eq!(
            posted(&github, &reply(Some("Fixed"))).as_deref(),
            Some("inline:9")
        );
        assert_eq!(
            posted(&forgejo, &reply(Some("Fixed"))).as_deref(),
            Some("reviews:4")
        );
        assert_eq!(
            posted(&gitlab, &reply(Some("Fixed"))).as_deref(),
            Some("discussion:9")
        );
        assert_eq!(posted(&forgejo, &reply(None)), None);
        assert_eq!(posted(&forgejo, &Action::Close), None);
        assert_eq!(forgejo.posted(&comment, &json!({})), None);
    }

    #[test]
    fn edits_patch_fields_replace_labels_and_change_reviewers() {
        for forge in forges() {
            let edit = Edit {
                title: Some("New".into()),
                base: Some("next".into()),
                add_labels: vec!["a".into(), "bug".into()],
                remove_labels: vec!["old".into()],
                add_reviewers: vec!["sam".into()],
                remove_reviewers: vec!["kim".into()],
                ..Edit::default()
            };
            let current = pr(&forge, false, &["bug", "old"]);
            assert_eq!(
                requests(&forge, &current, Action::Edit(edit)),
                [
                    (
                        "PATCH".into(),
                        "repos/team/repo/pulls/7".into(),
                        json!({"title": "New", "base": "next"})
                    ),
                    (
                        "PUT".into(),
                        "repos/team/repo/issues/7/labels".into(),
                        json!({"labels": ["bug", "a"]})
                    ),
                    (
                        "POST".into(),
                        "repos/team/repo/pulls/7/requested_reviewers".into(),
                        json!({"reviewers": ["sam"]})
                    ),
                    (
                        "DELETE".into(),
                        "repos/team/repo/pulls/7/requested_reviewers".into(),
                        json!({"reviewers": ["kim"]})
                    ),
                ]
            );
        }
    }

    #[test]
    fn draft_state_is_a_mutation_on_github_and_a_title_on_forgejo() {
        let [github, forgejo] = forges();
        let ready = Action::Edit(Edit {
            draft: Some(false),
            ..Edit::default()
        });
        let planned = requests(&github, &pr(&github, true, &[]), ready.clone());
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].1, "graphql");
        assert_eq!(planned[0].2["variables"], json!({"id": "PR_1"}));
        assert!(
            planned[0].2["query"]
                .as_str()
                .unwrap()
                .contains("markPullRequestReadyForReview")
        );
        assert_eq!(
            requests(&forgejo, &pr(&forgejo, true, &[]), ready.clone()),
            [(
                "PATCH".into(),
                "repos/team/repo/pulls/7".into(),
                json!({"title": "Fix"})
            )]
        );
        // Already ready: nothing to send.
        assert!(requests(&forgejo, &pr(&forgejo, false, &[]), ready).is_empty());
        let draft_with_title = Action::Edit(Edit {
            title: Some("[WIP] Other".into()),
            draft: Some(true),
            ..Edit::default()
        });
        assert_eq!(
            requests(&forgejo, &pr(&forgejo, false, &[]), draft_with_title)[0].2,
            json!({"title": "WIP: Other"})
        );
        // A new title alone keeps a Forgejo draft a draft, and a ready PR ready.
        let retitle = Action::Edit(Edit {
            title: Some("New".into()),
            ..Edit::default()
        });
        assert_eq!(
            requests(&forgejo, &pr(&forgejo, true, &[]), retitle.clone())[0].2,
            json!({"title": "WIP: New"})
        );
        assert_eq!(
            requests(&forgejo, &pr(&forgejo, false, &[]), retitle)[0].2,
            json!({"title": "New"})
        );
    }

    #[test]
    fn comments_state_changes_and_merges_use_each_forges_endpoints() {
        let [github, forgejo] = forges();
        for forge in [&github, &forgejo] {
            let open = pr(forge, false, &[]);
            assert_eq!(
                requests(
                    forge,
                    &open,
                    Action::Comment {
                        body: "hi".into(),
                        attachments: Vec::new()
                    }
                ),
                [(
                    "POST".into(),
                    "repos/team/repo/issues/7/comments".into(),
                    json!({"body": "hi"})
                )]
            );
            assert_eq!(
                requests(forge, &open, Action::Close),
                [(
                    "PATCH".into(),
                    "repos/team/repo/pulls/7".into(),
                    json!({"state": "closed"})
                )]
            );
            assert!(forge.plan(&open, &Action::Reopen).is_err());
        }
        let merge = Action::Merge {
            method: MergeMethod::Squash,
            delete_branch: true,
        };
        assert_eq!(
            requests(&github, &pr(&github, false, &[]), merge.clone()),
            [
                (
                    "PUT".into(),
                    "repos/team/repo/pulls/7/merge".into(),
                    json!({"merge_method": "squash"})
                ),
                (
                    "DELETE".into(),
                    "repos/team/repo/git/refs/heads/topic".into(),
                    Value::Null
                ),
            ]
        );
        assert_eq!(
            github.plan(&pr(&github, false, &[]), &merge).unwrap()[0].confirm,
            Some("merged")
        );
        let mut fork = pr(&github, false, &[]);
        fork.head_repository = Some("someone/repo".into());
        assert_eq!(requests(&github, &fork, merge.clone()).len(), 1);
        assert_eq!(
            requests(&forgejo, &pr(&forgejo, false, &[]), merge),
            [(
                "POST".into(),
                "repos/team/repo/pulls/7/merge".into(),
                json!({"Do": "squash", "delete_branch_after_merge": true})
            )]
        );
    }

    #[test]
    fn gitlab_changes_merge_requests_in_one_update_by_user_id() {
        let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
        let mut current = pr(&gitlab, false, &["bug"]);
        current.reviewers = vec!["kim".into(), "lee".into()];
        current.user_ids = [("kim".into(), 1), ("lee".into(), 2), ("sam".into(), 3)].into();
        let edit = Action::Edit(Edit {
            title: Some("New".into()),
            body: Some("Why".into()),
            base: Some("next".into()),
            add_labels: vec!["a".into(), "b".into()],
            remove_labels: vec!["bug".into()],
            add_reviewers: vec!["sam".into()],
            remove_reviewers: vec!["kim".into()],
            draft: Some(true),
            ..Edit::default()
        });
        assert_eq!(
            requests(&gitlab, &current, edit),
            [(
                "PUT".into(),
                "projects/group%2Fsub%2Frepo/merge_requests/7".into(),
                json!({"title": "Draft: New", "description": "Why", "target_branch": "next",
                    "add_labels": "a,b", "remove_labels": "bug", "reviewer_ids": [2, 3]})
            )]
        );
        let unknown = Action::Edit(Edit {
            add_reviewers: vec!["ann".into()],
            ..Edit::default()
        });
        assert!(gitlab.plan(&current, &unknown).is_err());
        let ready = Action::Edit(Edit {
            draft: Some(false),
            ..Edit::default()
        });
        let mut draft = pr(&gitlab, true, &[]);
        draft.item.title = "[Draft] Fix".into();
        assert_eq!(
            requests(&gitlab, &draft, ready)[0].2,
            json!({"title": "Fix"})
        );

        let open = pr(&gitlab, false, &[]);
        assert_eq!(
            requests(
                &gitlab,
                &open,
                Action::Comment {
                    body: "hi".into(),
                    attachments: Vec::new()
                }
            ),
            [(
                "POST".into(),
                "projects/group%2Fsub%2Frepo/merge_requests/7/notes".into(),
                json!({"body": "hi"})
            )]
        );
        assert_eq!(
            requests(&gitlab, &open, Action::Close)[0].2,
            json!({"state_event": "close"})
        );
        let merge = |method| Action::Merge {
            method,
            delete_branch: true,
        };
        assert_eq!(
            requests(&gitlab, &open, merge(MergeMethod::Squash)),
            [(
                "PUT".into(),
                "projects/group%2Fsub%2Frepo/merge_requests/7/merge".into(),
                json!({"squash": true, "should_remove_source_branch": true})
            )]
        );
        assert!(gitlab.plan(&open, &merge(MergeMethod::Rebase)).is_err());
    }

    #[test]
    fn actions_refuse_what_the_item_cannot_take() {
        assert!(
            Action::Edit(Edit::default())
                .validate(ItemKind::Pr)
                .is_err()
        );
        let base = Action::Edit(Edit {
            base: Some("main".into()),
            ..Edit::default()
        });
        assert!(base.validate(ItemKind::Issue).is_err());
        assert!(base.validate(ItemKind::Pr).is_ok());
        let merge = Action::Merge {
            method: MergeMethod::Merge,
            delete_branch: false,
        };
        assert!(merge.validate(ItemKind::Issue).is_err());
        assert!(
            Action::Comment {
                body: " ".into(),
                attachments: Vec::new()
            }
            .validate(ItemKind::Issue)
            .is_err()
        );
        let thread = |reply: Option<&str>, resolve| {
            Action::Thread(ThreadChange {
                comment: 5,
                reply: reply.map(str::to_owned),
                resolve,
            })
        };
        assert!(thread(Some("Why"), None).validate(ItemKind::Pr).is_ok());
        assert!(thread(None, Some(true)).validate(ItemKind::Pr).is_ok());
        assert!(thread(None, Some(true)).validate(ItemKind::Issue).is_err());
        assert!(thread(None, None).validate(ItemKind::Pr).is_err());
        assert!(
            thread(Some(" "), Some(true))
                .validate(ItemKind::Pr)
                .is_err()
        );
        assert_eq!(thread(None, Some(false)).name(), "unresolve");
    }
}
