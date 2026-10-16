//! GitLab's REST shapes: issues and merge requests numbered by `iid`,
//! projects addressed by their encoded path, and states of its own.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{
    ForgeRepo,
    account::Account,
    action::{Edit, MergeMethod},
    api::{HttpMethod, Request, encode},
    item::{Current, Item, ItemState, PrSummary, text},
    link::ItemKind,
    pr::state::Verdict,
    rest::RestCheck,
    thread::{Target, Thread},
    view::{Comment, Details, PrDetails, Review, ReviewComment},
};

/// Merge statuses during which GitLab has not yet decided mergeability.
const UNDECIDED: [&str; 4] = ["unchecked", "checking", "preparing", "approvals_syncing"];

/// Title prefixes that make a merge request a draft, including the former `WIP:`.
const DRAFT_PREFIXES: [&str; 5] = ["Draft:", "[Draft]", "(Draft)", "Draft -", "WIP:"];

impl ForgeRepo {
    /// An issue or merge request as GitLab reports it.
    pub(super) fn gitlab_current(
        &self,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        let state = match value["state"].as_str() {
            // A merge request is locked while GitLab merges it.
            Some("opened" | "locked") => ItemState::Open,
            Some("closed") => ItemState::Closed,
            Some("merged") if kind == ItemKind::Pr => ItemState::Merged,
            state => bail!("unrecognized GitLab state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: text(&value["source_branch"])
                    .context("merge request has no source branch")?,
                base: text(&value["target_branch"])
                    .context("merge request has no target branch")?,
                draft: value["draft"]
                    .as_bool()
                    .or(value["work_in_progress"].as_bool())
                    .unwrap_or_default(),
            }),
        };
        let merge_status = value["detailed_merge_status"].as_str().unwrap_or_default();
        let conflict = match (kind, value["has_conflicts"].as_bool()) {
            (ItemKind::Pr, Some(conflicts)) if !UNDECIDED.contains(&merge_status) => {
                Some(conflicts)
            }
            _ => None,
        };
        let same_project = value["source_project_id"] == value["target_project_id"];
        let reviewers: Vec<(String, u64)> = value["reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|user| Some((text(&user["username"])?, user["id"].as_u64()?)))
            .collect();
        Ok(Current {
            item: Item {
                kind,
                number,
                url: self.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: labels(&value["labels"]),
                pr,
            },
            node_id: None,
            head_repository: (kind == ItemKind::Pr && same_project).then(|| self.path.clone()),
            body: text(&value["description"]).unwrap_or_default(),
            head_sha: text(&value["sha"]),
            conflict,
            reviewers: reviewers.iter().map(|(name, _)| name.clone()).collect(),
            user_ids: reviewers.into_iter().collect(),
        })
    }
}

impl ForgeRepo {
    /// Look up the IDs of the users `names` that `current` has none for.
    pub(super) async fn gitlab_user_ids(
        &self,
        account: &Account,
        names: &[String],
        current: &mut Current,
    ) -> Result<()> {
        for name in names {
            if current.user_ids.contains_key(name) {
                continue;
            }
            let request = Request::get(format!("users?username={}", encode(name, false)));
            let users = self.send(account, &request).await?;
            let id = users
                .as_array()
                .into_iter()
                .flatten()
                .find(|user| {
                    user["username"]
                        .as_str()
                        .is_some_and(|username| username.eq_ignore_ascii_case(name))
                })
                .and_then(|user| user["id"].as_u64())
                .with_context(|| format!("GitLab has no user {name}"))?;
            current.user_ids.insert(name.clone(), id);
        }
        Ok(())
    }

    /// One update with every changed field; GitLab adds and removes labels by
    /// name, keeps draft state in the title and replaces reviewers by ID.
    pub(super) fn gitlab_edit(&self, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let item = &current.item;
        let mut fields = serde_json::Map::new();
        let mut title = edit.title.clone();
        if let Some(pr) = &item.pr {
            let draft = edit.draft.unwrap_or(pr.draft);
            if title.is_some() || draft != pr.draft {
                let current = title.as_deref().unwrap_or(&item.title);
                title = Some(draft_title(current, draft));
            }
        }
        if let Some(title) = title {
            fields.insert("title".into(), title.into());
        }
        if let Some(body) = &edit.body {
            fields.insert("description".into(), body.clone().into());
        }
        if let Some(base) = &edit.base {
            fields.insert("target_branch".into(), base.clone().into());
        }
        for (field, labels) in [
            ("add_labels", &edit.add_labels),
            ("remove_labels", &edit.remove_labels),
        ] {
            if !labels.is_empty() {
                fields.insert(field.into(), labels.join(",").into());
            }
        }
        if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
            fields.insert("reviewer_ids".into(), reviewer_ids(current, edit)?.into());
        }
        if fields.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Request::new(
            HttpMethod::Put,
            self.item_endpoint(item.kind, item.number),
            Some(fields.into()),
        )])
    }

    /// The latest status of each CI job and external check on `head`.
    pub(super) async fn gitlab_checks(
        &self,
        account: &Account,
        head: &str,
    ) -> Result<Vec<RestCheck>> {
        let endpoint = self.repo_endpoint(&format!("repository/commits/{head}/statuses"));
        statuses(&self.pages(account, &endpoint).await?)
    }

    /// Reviewers' states, with every approval as an approving review.
    pub(super) async fn gitlab_verdicts(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let merge_request = self.item_endpoint(ItemKind::Pr, number);
        let reviewers = Request::get(format!("{merge_request}/reviewers"));
        let approvals = Request::get(format!("{merge_request}/approvals"));
        let (reviewers, approvals) = tokio::try_join!(
            self.send(account, &reviewers),
            self.send(account, &approvals)
        )?;
        verdicts(&reviewers, &approvals)
    }

    /// The item's notes that people wrote, without GitLab's system notes.
    pub(super) async fn gitlab_notes(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<Value>> {
        let endpoint = format!("{}/notes", self.item_endpoint(kind, number));
        let mut notes = self.pages(account, &endpoint).await?;
        notes.retain(|note| note["system"] != true);
        Ok(notes)
    }

    pub(super) async fn gitlab_details(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Details> {
        let value = self
            .send(account, &Request::get(self.item_endpoint(kind, number)))
            .await?;
        let current = self.current(kind, number, &value)?;
        let pr = match current.item.pr {
            Some(pr) => Some(PrDetails::new(pr.head, pr.base, pr.draft)),
            None => None,
        };
        Ok(Details {
            number,
            title: current.item.title,
            state: current.item.state.as_str().into(),
            author: username(&value["author"]),
            created_at: text(&value["created_at"]).unwrap_or_default(),
            labels: current.item.labels,
            body: current.body,
            pr,
            comments: None,
            reviews: None,
        })
    }

    /// Fills the item's comments and returns a merge request's reviews: one
    /// per diff thread, and one per approval or change request.
    pub(super) async fn gitlab_discussion(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        let endpoint = format!("{}/discussions", self.item_endpoint(kind, number));
        let (comments, mut reviews) = discussions(&self.pages(account, &endpoint).await?);
        details.comments = Some(comments);
        if kind == ItemKind::Issue {
            return Ok(None);
        }
        let verdicts = self.gitlab_verdicts(account, number).await?;
        reviews.extend(verdicts.into_iter().filter_map(|(author, verdict)| {
            let state = match verdict {
                Verdict::Approved => "approved",
                Verdict::ChangesRequested => "changes_requested",
                Verdict::Commented => return None,
            };
            Some(Review {
                author,
                state: state.into(),
                submitted_at: String::new(),
                body: String::new(),
                comments: Vec::new(),
            })
        }));
        Ok(Some(reviews))
    }
}

/// GitLab merges with the project's merge method, optionally squashing.
pub(super) fn gitlab_merge(
    endpoint: String,
    method: MergeMethod,
    delete_branch: bool,
) -> Result<Request> {
    ensure!(
        method != MergeMethod::Rebase,
        "GitLab merges with the project's merge method; use merge or squash"
    );
    Ok(Request::new(
        HttpMethod::Put,
        endpoint,
        Some(json!({
            "squash": method == MergeMethod::Squash,
            "should_remove_source_branch": delete_branch,
        })),
    ))
}

/// The title with GitLab's draft prefix added or removed.
pub(super) fn draft_title(title: &str, draft: bool) -> String {
    let bare = DRAFT_PREFIXES
        .iter()
        .find_map(|prefix| {
            title
                .get(..prefix.len())
                .filter(|start| start.eq_ignore_ascii_case(prefix))
                .map(|_| title[prefix.len()..].trim_start())
        })
        .unwrap_or(title);
    if draft {
        format!("Draft: {bare}")
    } else {
        bare.to_owned()
    }
}

/// The reviewers after the edit, by user ID.
fn reviewer_ids(current: &Current, edit: &Edit) -> Result<Vec<u64>> {
    let mut names: Vec<&String> = current
        .reviewers
        .iter()
        .filter(|name| !edit.remove_reviewers.contains(name))
        .collect();
    for name in &edit.add_reviewers {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            current
                .user_ids
                .get(name)
                .copied()
                .with_context(|| format!("GitLab user {name} was not looked up"))
        })
        .collect()
}

/// The newest status of each name; retried jobs keep their earlier ones.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    let mut latest = BTreeMap::new();
    for status in statuses {
        let name = status["name"]
            .as_str()
            .context("GitLab status has no name")?;
        let id = status["id"].as_u64().context("GitLab status has no ID")?;
        let state = match status["status"].as_str() {
            Some(
                "created"
                | "waiting_for_resource"
                | "preparing"
                | "pending"
                | "running"
                | "scheduled"
                | "canceling"
                | "waiting_for_callback",
            ) => "Pending",
            Some("success") => "Success",
            Some("failed") if status["allow_failure"] == true => "Warning",
            Some("failed") => "Failure",
            Some("canceled") => "Cancelled",
            // A manual job waits for someone to start it, which CI cannot.
            Some("skipped" | "manual") => "Skipped",
            state => bail!("unknown GitLab CI state {state:?}"),
        };
        if latest.get(name).is_none_or(|(newest, _)| *newest < id) {
            latest.insert(name.to_owned(), (id, state));
        }
    }
    Ok(latest
        .into_iter()
        .map(|(name, (_, state))| RestCheck { name, state })
        .collect())
}

/// Reviewers' latest states, then approvals, which outrank them.
fn verdicts(reviewers: &Value, approvals: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for reviewer in reviewers.as_array().context("reviewers are not a list")? {
        let verdict = match reviewer["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("requested_changes") => Verdict::ChangesRequested,
            Some("reviewed") => Verdict::Commented,
            Some("unreviewed" | "unapproved" | "review_started") => continue,
            state => bail!("unknown GitLab reviewer state {state:?}"),
        };
        verdicts.push((username(&reviewer["user"]), verdict));
    }
    for approval in approvals["approved_by"]
        .as_array()
        .context("approvals have no approvers")?
    {
        verdicts.push((username(&approval["user"]), Verdict::Approved));
    }
    Ok(verdicts)
}

/// Comments, and a review for each thread on the diff.
fn discussions(discussions: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let mut comments = Vec::new();
    let mut reviews = Vec::new();
    for discussion in discussions {
        let notes = notes(discussion);
        let Some(first) = notes.first() else { continue };
        if first["type"] == "DiffNote" {
            reviews.push(Review {
                author: username(&first["author"]),
                state: "commented".into(),
                submitted_at: text(&first["created_at"]).unwrap_or_default(),
                body: String::new(),
                comments: notes
                    .iter()
                    .map(|note| review_comment(note, first["id"].as_u64().unwrap_or_default()))
                    .collect(),
            });
        } else {
            comments.extend(notes.iter().map(|note| Comment {
                author: username(&note["author"]),
                created_at: text(&note["created_at"]).unwrap_or_default(),
                body: text(&note["body"]).unwrap_or_default(),
            }));
        }
    }
    (comments, reviews)
}

/// The discussion with note `comment`, which replies and resolution name.
pub(super) fn gitlab_thread(discussions: &[Value], comment: u64) -> Option<Thread> {
    discussions.iter().find_map(|discussion| {
        let notes = notes(discussion);
        notes.iter().find(|note| note["id"] == comment)?;
        let first = notes.first()?;
        Some(Thread {
            first: first["id"].as_u64()?,
            replies: notes.len() - 1,
            resolved: first["resolved"].as_bool(),
            target: Target::GitLab {
                discussion: text(&discussion["id"])?,
            },
        })
    })
}

/// A discussion's notes without the ones GitLab writes for events.
fn notes(discussion: &Value) -> Vec<&Value> {
    discussion["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|note| note["system"] != true)
        .collect()
}

/// A diff note on the new side, or the old side of a removed line.
fn review_comment(note: &Value, thread: u64) -> ReviewComment {
    let position = &note["position"];
    ReviewComment {
        id: note["id"].as_u64().unwrap_or_default(),
        thread,
        author: username(&note["author"]),
        created_at: text(&note["created_at"]).unwrap_or_default(),
        path: text(&position["new_path"])
            .or_else(|| text(&position["old_path"]))
            .unwrap_or_default(),
        line: position["new_line"]
            .as_u64()
            .or(position["old_line"].as_u64()),
        body: text(&note["body"]).unwrap_or_default(),
        resolved: note["resolved"].as_bool(),
    }
}

/// A deleted account has no user.
fn username(user: &Value) -> String {
    user["username"].as_str().unwrap_or("ghost").to_owned()
}

/// Label names, listed as strings unless details were requested.
fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| text(label).or_else(|| text(&label["name"])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn gitlab() -> ForgeRepo {
        ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap()
    }

    #[test]
    fn merge_requests_read_branches_drafts_and_conflicts() {
        let merge_request = json!({"iid": 7, "title": "Draft: Fix", "state": "opened",
            "description": "Why", "labels": ["bug"], "source_branch": "topic",
            "target_branch": "main", "draft": true, "sha": "abc", "has_conflicts": true,
            "detailed_merge_status": "conflict", "source_project_id": 1, "target_project_id": 1});
        let current = gitlab().current(ItemKind::Pr, 7, &merge_request).unwrap();
        assert_eq!(
            current.item,
            Item {
                kind: ItemKind::Pr,
                number: 7,
                url: "https://gitlab.com/group/sub/project/-/merge_requests/7".into(),
                title: "Draft: Fix".into(),
                state: ItemState::Open,
                labels: vec!["bug".into()],
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: true,
                }),
            }
        );
        assert_eq!(current.conflict, Some(true));
        assert_eq!(current.head_sha.as_deref(), Some("abc"));
        assert_eq!(
            current.head_repository.as_deref(),
            Some("group/sub/project")
        );
        assert_eq!(current.body, "Why");

        let mut checking = merge_request.clone();
        checking["detailed_merge_status"] = json!("checking");
        checking["source_project_id"] = json!(2);
        let current = gitlab().current(ItemKind::Pr, 7, &checking).unwrap();
        assert_eq!(current.conflict, None);
        assert_eq!(current.head_repository, None);

        let mut merged = merge_request;
        merged["state"] = json!("merged");
        let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
        assert_eq!(current.item.state, ItemState::Merged);
        assert!(gitlab().current(ItemKind::Pr, 8, &merged).is_err());
    }

    #[test]
    fn statuses_keep_the_newest_of_each_name_in_shared_states() {
        let listed = json!([
            {"id": 1, "name": "rust", "status": "failed"},
            {"id": 3, "name": "rust", "status": "running"},
            {"id": 2, "name": "lint", "status": "failed", "allow_failure": true},
            {"id": 4, "name": "deploy", "status": "manual"},
            {"id": 5, "name": "external", "status": "canceled"}
        ]);
        let checks = statuses(listed.as_array().unwrap()).unwrap();
        assert_eq!(
            checks
                .iter()
                .map(|check| (check.name.as_str(), check.state))
                .collect::<Vec<_>>(),
            [
                ("deploy", "Skipped"),
                ("external", "Cancelled"),
                ("lint", "Warning"),
                ("rust", "Pending")
            ]
        );
        assert!(statuses(&[json!({"id": 1, "name": "x", "status": "later"})]).is_err());
    }

    #[test]
    fn approvals_and_reviewer_states_become_verdicts() {
        let reviewers = json!([
            {"user": {"username": "ann"}, "state": "requested_changes"},
            {"user": {"username": "bob"}, "state": "reviewed"},
            {"user": {"username": "cat"}, "state": "unreviewed"}
        ]);
        let approvals = json!({"approved_by": [{"user": {"username": "dan"}}]});
        assert_eq!(
            verdicts(&reviewers, &approvals).unwrap(),
            [
                ("ann".to_owned(), Verdict::ChangesRequested),
                ("bob".to_owned(), Verdict::Commented),
                ("dan".to_owned(), Verdict::Approved)
            ]
        );
        assert!(verdicts(&json!([{"state": "later"}]), &approvals).is_err());
        assert!(verdicts(&json!([]), &json!({})).is_err());
    }

    #[test]
    fn diff_threads_become_reviews_and_other_notes_comments() {
        let listed = json!([
            {"notes": [{"system": true, "body": "added 1 commit"}]},
            {"notes": [{"author": {"username": "ann"}, "body": "Looks fine", "created_at": "t1"}]},
            {"notes": [
                {"id": 30, "type": "DiffNote", "author": {"username": "bob"}, "body": "Bug", "created_at": "t2",
                    "resolved": false, "position": {"new_path": "a.rs", "new_line": 3}},
                {"id": 31, "type": "DiffNote", "author": null, "body": "Fixed", "created_at": "t3",
                    "resolved": true, "position": {"old_path": "b.rs", "new_line": null, "old_line": 9}}
            ]}
        ]);
        let (comments, reviews) = discussions(listed.as_array().unwrap());
        assert_eq!(comments.len(), 1);
        assert_eq!(
            (comments[0].author.as_str(), comments[0].body.as_str()),
            ("ann", "Looks fine")
        );
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            (reviews[0].author.as_str(), reviews[0].state.as_str()),
            ("bob", "commented")
        );
        let threads = &reviews[0].comments;
        assert_eq!(
            (
                threads[0].path.as_str(),
                threads[0].line,
                threads[0].resolved
            ),
            ("a.rs", Some(3), Some(false))
        );
        assert_eq!(
            (
                threads[1].author.as_str(),
                threads[1].path.as_str(),
                threads[1].line
            ),
            ("ghost", "b.rs", Some(9))
        );
        assert_eq!(
            threads.iter().map(|c| (c.id, c.thread)).collect::<Vec<_>>(),
            [(30, 30), (31, 30)]
        );
    }

    #[test]
    fn notes_find_their_discussion() {
        let listed = json!([
            {"id": "d1", "notes": [{"id": 1, "system": true}, {"id": 2, "resolved": false}, {"id": 3}]},
            {"id": "d2", "notes": [{"id": 4, "resolved": true}]}
        ]);
        let listed = listed.as_array().unwrap();
        let thread = gitlab_thread(listed, 3).unwrap();
        assert_eq!(
            (thread.first, thread.replies, thread.resolved),
            (2, 1, Some(false))
        );
        assert_eq!(
            thread.target,
            Target::GitLab {
                discussion: "d1".into()
            }
        );
        assert_eq!(gitlab_thread(listed, 4).unwrap().resolved, Some(true));
        // System notes are events, not comments.
        assert!(gitlab_thread(listed, 1).is_none());
    }

    #[test]
    fn issues_use_their_own_numbers_routes_and_states() {
        let issue = json!({"iid": 3, "id": 900, "title": "Bug", "state": "closed",
            "labels": [{"name": "type/bug"}]});
        let current = gitlab().current(ItemKind::Issue, 3, &issue).unwrap();
        assert_eq!(
            current.item.url,
            "https://gitlab.com/group/sub/project/-/issues/3"
        );
        assert_eq!(current.item.state, ItemState::Closed);
        assert_eq!(current.item.labels, ["type/bug"]);
        assert!(gitlab().current(ItemKind::Issue, 900, &issue).is_err());
        let mut merged = issue;
        merged["state"] = json!("merged");
        assert!(gitlab().current(ItemKind::Issue, 3, &merged).is_err());
    }
}
