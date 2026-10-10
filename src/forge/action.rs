//! Changes to an issue or PR, planned as the REST requests each forge needs
//! and applied in order, stopping at the first failure.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
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
    },
    Close,
    Reopen,
    Merge {
        method: MergeMethod,
        delete_branch: bool,
    },
}

/// Fields left empty stay as they are; base, reviewers and draft state are
/// PR-only.
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
                    "nothing to change; pass a new title, description, base, label, reviewer or draft state"
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
            Self::Comment { body } => ensure!(!body.trim().is_empty(), "the comment is empty"),
            Self::Merge { .. } => pr_only("merge")?,
            Self::Close | Self::Reopen => {}
        }
        Ok(())
    }
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
        action.validate(kind)?;
        let current = self.read(account, kind, number).await?;
        for request in self.plan(&current, action)? {
            self.send(account, &request).await?;
        }
        Ok(self.read(account, kind, number).await?.item)
    }

    pub(crate) async fn read(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Current> {
        let value = self
            .send(account, &Request::get(self.item_endpoint(kind, number)))
            .await?;
        self.current(kind, number, &value)
    }

    /// The requests that carry out `action` on `current`, in order.
    pub(crate) fn plan(&self, current: &Current, action: &Action) -> Result<Vec<Request>> {
        let item = &current.item;
        let endpoint = |path: &str| self.repo_endpoint(&format!("issues/{}/{path}", item.number));
        let pulls = |path: &str| self.repo_endpoint(&format!("pulls/{}/{path}", item.number));
        let patch = |body: Value| {
            Request::new(
                HttpMethod::Patch,
                self.item_endpoint(item.kind, item.number),
                Some(body),
            )
        };
        Ok(match action {
            Action::Edit(edit) => self.plan_edit(current, edit)?,
            Action::Comment { body } => vec![Request::new(
                HttpMethod::Post,
                endpoint("comments"),
                Some(json!({ "body": body })),
            )],
            Action::Close => {
                ensure!(
                    item.state == ItemState::Open,
                    "#{} is not open",
                    item.number
                );
                vec![patch(json!({ "state": "closed" }))]
            }
            Action::Reopen => {
                ensure!(
                    item.state == ItemState::Closed,
                    "#{} is not closed, or was merged",
                    item.number
                );
                vec![patch(json!({ "state": "open" }))]
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
                let pr = item.pr.as_ref().context("only PRs merge")?;
                match self.kind {
                    ForgeKind::GitHub => {
                        let mut requests = vec![Request::new(
                            HttpMethod::Put,
                            pulls("merge"),
                            Some(json!({ "merge_method": method.as_str() })),
                        )];
                        // A fork's branch is not this repository's to delete.
                        if *delete_branch
                            && current.head_repository.as_deref() == Some(self.path.as_str())
                        {
                            requests.push(Request::new(
                                HttpMethod::Delete,
                                self.repo_endpoint(&format!("git/refs/heads/{}", pr.head)),
                                None,
                            ));
                        }
                        requests
                    }
                    ForgeKind::Forgejo => vec![Request::new(
                        HttpMethod::Post,
                        pulls("merge"),
                        Some(json!({
                            "Do": method.as_str(),
                            "delete_branch_after_merge": delete_branch,
                        })),
                    )],
                }
            }
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
        if let (Some(draft), ForgeKind::Forgejo) = (draft, self.kind) {
            let current = title.as_deref().unwrap_or(&item.title);
            title = Some(draft_title(current, draft));
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
    use crate::forge::item::PrSummary;

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
    }

    #[test]
    fn comments_state_changes_and_merges_use_each_forges_endpoints() {
        let [github, forgejo] = forges();
        for forge in [&github, &forgejo] {
            let open = pr(forge, false, &[]);
            assert_eq!(
                requests(forge, &open, Action::Comment { body: "hi".into() }),
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
            Action::Comment { body: " ".into() }
                .validate(ItemKind::Issue)
                .is_err()
        );
    }
}
