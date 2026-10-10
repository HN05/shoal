//! An issue's or PR's content, status and discussion, read on request so
//! callers need not know whether the forge is GitHub or Forgejo.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use futures_util::future::try_join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ForgeKind, ForgeRepo, Query,
    link::ItemKind,
    pr::state::{CheckResult, PrStatus, ReviewState},
};

/// The items a view selects and the worktree whose origin and forge login
/// look them up. Lookups run in the CLI: discussion can exceed a protocol frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selected {
    pub path: PathBuf,
    pub items: Vec<SelectedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedItem {
    pub url: String,
    pub kind: ItemKind,
}

impl Selected {
    /// Each item as the forge reports it now, in selection order.
    pub(crate) async fn view(&self, comments: bool) -> Result<Vec<ItemView>> {
        let remote = super::repository::remote_url_from_path(&self.path)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        Ok(futures_util::future::join_all(
            self.items
                .iter()
                .map(|item| forge.view(&self.path, item.kind, &item.url, comments)),
        )
        .await)
    }
}

/// A failed lookup leaves `details` empty, or the discussion it would have
/// filled, and records its error instead of failing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemView {
    pub url: String,
    pub kind: ItemKind,
    #[serde(flatten)]
    pub details: Option<Details>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Details {
    pub number: u64,
    pub title: String,
    /// `open`, `closed`, or `merged` for a PR.
    pub state: String,
    pub author: String,
    pub created_at: String,
    pub labels: Vec<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrDetails>,
    /// Absent when comments were not requested or their lookup failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<Comment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<Vec<Review>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDetails {
    pub head: String,
    pub base: String,
    pub draft: bool,
    /// Unknown while the forge computes mergeability, and for drafts and closed PRs.
    pub merge_conflicts: Option<bool>,
    pub checks: Vec<CheckResult>,
    pub review: Option<ReviewState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub author: String,
    /// `approved`, `changes_requested`, `commented` or `dismissed`.
    pub state: String,
    pub submitted_at: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub author: String,
    pub created_at: String,
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
    /// Unknown on GitHub, whose REST API does not report thread resolution.
    pub resolved: Option<bool>,
}

impl ForgeRepo {
    /// The item at `url` with its discussion when `comments` is set; PRs also
    /// carry the state `shoal status` reports.
    pub(crate) async fn view(
        &self,
        path: &Path,
        kind: ItemKind,
        url: &str,
        comments: bool,
    ) -> ItemView {
        let mut errors = Vec::new();
        let details = match self.details(path, kind, url, comments, &mut errors).await {
            Ok(details) => Some(details),
            Err(error) => {
                errors.insert(0, format!("{error:#}"));
                None
            }
        };
        ItemView {
            url: url.into(),
            kind,
            details,
            errors,
        }
    }

    async fn details(
        &self,
        path: &Path,
        kind: ItemKind,
        url: &str,
        comments: bool,
        errors: &mut Vec<String>,
    ) -> Result<Details> {
        let number = match kind {
            ItemKind::Issue => self.issue(url)?.0,
            ItemKind::Pr => self.pull(url)?.0,
        };
        let content = async {
            match self.kind {
                ForgeKind::GitHub => self.github_details(path, kind, number, comments).await,
                ForgeKind::Forgejo => self.forgejo_details(kind, number).await,
            }
        };
        let status = async {
            match kind {
                ItemKind::Pr => Some(self.pr_status(path, url).await),
                ItemKind::Issue => None,
            }
        };
        let (content, status) = tokio::join!(content, status);
        let mut details = content?;
        if let (Some(pr), Some(status)) = (&mut details.pr, status) {
            pr.record(status, errors);
        }
        if comments {
            let discussion = match self.kind {
                ForgeKind::GitHub => self.github_reviews(path, kind, number).await,
                ForgeKind::Forgejo => self.forgejo_discussion(kind, number, &mut details).await,
            };
            match discussion {
                Ok(Some(reviews)) => details.reviews = Some(reviews),
                Ok(None) => {}
                Err(error) => errors.push(format!("comments lookup failed: {error:#}")),
            }
        }
        Ok(details)
    }

    async fn github_details(
        &self,
        path: &Path,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details> {
        let id = number.to_string();
        let repository = format!("{}/{}", self.host, self.path);
        let (command, extra) = match kind {
            ItemKind::Issue => ("issue", ""),
            ItemKind::Pr => ("pr", ",headRefName,baseRefName,isDraft"),
        };
        let fields = format!(
            "number,title,body,state,author,labels,createdAt{extra}{}",
            if comments { ",comments" } else { "" }
        );
        let args = [
            command,
            "view",
            &id,
            "--repo",
            &repository,
            "--json",
            &fields,
        ];
        let output = self.kind.query(path, &args, Query::View).await?;
        github_details(&output, kind, number)
    }

    /// Reviews with their inline comments; issues have none.
    async fn github_reviews(
        &self,
        path: &Path,
        kind: ItemKind,
        number: u64,
    ) -> Result<Option<Vec<Review>>> {
        if kind == ItemKind::Issue {
            return Ok(None);
        }
        let pages = |resource: &'static str| {
            let endpoint = format!("repos/{}/pulls/{number}/{resource}", self.path);
            async move {
                let args = [
                    "api",
                    "--hostname",
                    &self.host,
                    &endpoint,
                    "--paginate",
                    "--slurp",
                ];
                let output = self.kind.query(path, &args, Query::View).await?;
                let pages: Vec<Vec<Value>> = serde_json::from_str(&output)
                    .with_context(|| format!("invalid gh {resource} response"))?;
                Ok::<_, anyhow::Error>(pages.into_iter().flatten().collect::<Vec<_>>())
            }
        };
        let (reviews, comments) = tokio::try_join!(pages("reviews"), pages("comments"))?;
        github_reviews(&reviews, &comments).map(Some)
    }

    async fn forgejo_details(&self, kind: ItemKind, number: u64) -> Result<Details> {
        let item = match kind {
            ItemKind::Issue => self.forgejo_api(&format!("issues/{number}")).await?,
            ItemKind::Pr => self.forgejo_api(&format!("pulls/{number}")).await?,
        };
        forgejo_details(&item, kind, number)
    }

    /// Fills the item's comments and returns a PR's reviews.
    async fn forgejo_discussion(
        &self,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        let comments = self
            .forgejo_api(&format!("issues/{number}/comments"))
            .await?;
        details.comments = Some(forgejo_comments(&comments)?);
        if kind == ItemKind::Issue {
            return Ok(None);
        }
        let reviews = self
            .forgejo_api_pages(&format!("pulls/{number}/reviews"))
            .await?;
        let reviews = try_join_all(reviews.into_iter().map(|review| async move {
            let id = review["id"].as_u64().context("Forgejo review has no ID")?;
            let comments = if review["comments_count"].as_u64().unwrap_or_default() > 0 {
                self.forgejo_api(&format!("pulls/{number}/reviews/{id}/comments"))
                    .await?
            } else {
                Value::Array(Vec::new())
            };
            Ok::<_, anyhow::Error>((review, comments))
        }))
        .await?;
        forgejo_reviews(&reviews).map(Some)
    }
}

impl PrDetails {
    fn record(&mut self, status: PrStatus, errors: &mut Vec<String>) {
        self.merge_conflicts = status.merge_conflicts;
        self.checks = status.checks;
        self.review = status.review;
        errors.extend(status.errors);
    }

    fn new(head: &Value, base: &Value, draft: bool) -> Result<Self> {
        Ok(Self {
            head: head.as_str().context("PR has no head branch")?.into(),
            base: base.as_str().context("PR has no base branch")?.into(),
            draft,
            merge_conflicts: None,
            checks: Vec::new(),
            review: None,
        })
    }
}

fn github_details(text: &str, kind: ItemKind, number: u64) -> Result<Details> {
    let item: Value = serde_json::from_str(text).context("invalid gh response")?;
    ensure!(item["number"] == number, "gh returned a different item");
    let pr = match kind {
        ItemKind::Issue => None,
        ItemKind::Pr => Some(PrDetails::new(
            &item["headRefName"],
            &item["baseRefName"],
            item["isDraft"].as_bool().unwrap_or_default(),
        )?),
    };
    let comments = match item.get("comments") {
        Some(comments) => Some(
            comments
                .as_array()
                .context("gh comments are not a list")?
                .iter()
                .map(|comment| Comment {
                    author: login(&comment["author"]),
                    created_at: text_field(&comment["createdAt"]),
                    body: text_field(&comment["body"]),
                })
                .collect(),
        ),
        None => None,
    };
    Ok(Details {
        number,
        title: required(&item["title"], "title")?,
        state: required(&item["state"], "state")?.to_ascii_lowercase(),
        author: login(&item["author"]),
        created_at: text_field(&item["createdAt"]),
        labels: labels(&item["labels"]),
        body: text_field(&item["body"]),
        pr,
        comments,
        reviews: None,
    })
}

/// REST reviews and inline comments; each comment belongs to the review that
/// posted it, including replies.
fn github_reviews(reviews: &[Value], comments: &[Value]) -> Result<Vec<Review>> {
    let mut result = Vec::new();
    for review in reviews {
        let Some(state) = review_state(review["state"].as_str())? else {
            continue;
        };
        let id = review["id"].as_u64().context("GitHub review has no ID")?;
        result.push(Review {
            author: login(&review["user"]),
            state,
            submitted_at: text_field(&review["submitted_at"]),
            body: text_field(&review["body"]),
            comments: comments
                .iter()
                .filter(|comment| comment["pull_request_review_id"].as_u64() == Some(id))
                .map(|comment| {
                    review_comment(comment, &comment["line"], &comment["original_line"], None)
                })
                .collect(),
        });
    }
    Ok(result)
}

fn forgejo_details(item: &Value, kind: ItemKind, number: u64) -> Result<Details> {
    ensure!(
        item["number"] == number,
        "Forgejo returned a different item"
    );
    let pr = match kind {
        ItemKind::Issue => {
            ensure!(
                item["pull_request"].is_null(),
                "#{number} is a PR, not an issue"
            );
            None
        }
        ItemKind::Pr => Some(PrDetails::new(
            &item["head"]["ref"],
            &item["base"]["ref"],
            item["draft"].as_bool().unwrap_or_default(),
        )?),
    };
    let state = if item["merged"].as_bool() == Some(true) {
        "merged".to_owned()
    } else {
        required(&item["state"], "state")?
    };
    Ok(Details {
        number,
        title: required(&item["title"], "title")?,
        state,
        author: login(&item["user"]),
        created_at: text_field(&item["created_at"]),
        labels: labels(&item["labels"]),
        body: text_field(&item["body"]),
        pr,
        comments: None,
        reviews: None,
    })
}

fn forgejo_comments(comments: &Value) -> Result<Vec<Comment>> {
    Ok(comments
        .as_array()
        .context("Forgejo comments are not a list")?
        .iter()
        .map(|comment| Comment {
            author: login(&comment["user"]),
            created_at: text_field(&comment["created_at"]),
            body: text_field(&comment["body"]),
        })
        .collect())
}

/// Each review with the inline comments Forgejo lists under it, replies
/// included. Forgejo records resolution on a thread's first comment only, so
/// it applies to every comment on the same line.
fn forgejo_reviews(reviews: &[(Value, Value)]) -> Result<Vec<Review>> {
    let mut result = Vec::new();
    for (review, comments) in reviews {
        let Some(state) = review_state(review["state"].as_str())? else {
            continue;
        };
        let comments = comments
            .as_array()
            .context("Forgejo review comments are not a list")?
            .iter()
            .map(|comment| {
                let resolved = !comment["resolver"].is_null();
                let comment = review_comment(
                    comment,
                    &comment["position"],
                    &comment["original_position"],
                    None,
                );
                (comment, resolved)
            })
            .collect::<Vec<_>>();
        let resolved = comments
            .iter()
            .filter(|(_, resolved)| *resolved)
            .map(|(comment, _)| (comment.path.clone(), comment.line))
            .collect::<Vec<_>>();
        let comments = comments
            .into_iter()
            .map(|(mut comment, _)| {
                comment.resolved = Some(resolved.contains(&(comment.path.clone(), comment.line)));
                comment
            })
            .collect();
        result.push(Review {
            author: login(&review["user"]),
            state,
            submitted_at: text_field(&review["submitted_at"]),
            body: text_field(&review["body"]),
            comments,
        });
    }
    Ok(result)
}

/// A submitted review's state; pending drafts and review requests are not reviews.
fn review_state(state: Option<&str>) -> Result<Option<String>> {
    Ok(Some(
        match state.context("review has no state")? {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" | "REQUEST_CHANGES" => "changes_requested",
            "COMMENTED" | "COMMENT" => "commented",
            "DISMISSED" => "dismissed",
            "PENDING" | "REQUEST_REVIEW" => return Ok(None),
            other => anyhow::bail!("unknown review state {other}"),
        }
        .into(),
    ))
}

/// The line on the current side, or the original line of an outdated comment.
fn review_comment(
    comment: &Value,
    line: &Value,
    original: &Value,
    resolved: Option<bool>,
) -> ReviewComment {
    let positive = |value: &Value| value.as_u64().filter(|line| *line > 0);
    ReviewComment {
        author: login(&comment["user"]),
        created_at: text_field(&comment["created_at"]),
        path: text_field(&comment["path"]),
        line: positive(line).or_else(|| positive(original)),
        body: text_field(&comment["body"]),
        resolved,
    }
}

fn required(value: &Value, field: &str) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("item has no {field}"))
}

fn text_field(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// A deleted account has no user.
fn login(user: &Value) -> String {
    user["login"].as_str().unwrap_or("ghost").to_owned()
}

fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| label["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn github_issue_and_pr_details_read_content_and_comments() {
        let issue = json!({"number": 4, "title": "Crash", "body": "Steps", "state": "OPEN",
            "author": {"login": "ann"}, "labels": [{"name": "bug"}], "createdAt": "2026-01-01T00:00:00Z",
            "comments": [{"author": {"login": "bob"}, "createdAt": "2026-01-02T00:00:00Z", "body": "Same here"}]});
        let details = github_details(&issue.to_string(), ItemKind::Issue, 4).unwrap();
        assert_eq!(details.title, "Crash");
        assert_eq!(details.state, "open");
        assert_eq!(details.author, "ann");
        assert_eq!(details.labels, ["bug"]);
        assert!(details.pr.is_none());
        assert_eq!(details.comments.unwrap()[0].body, "Same here");
        assert!(github_details(&issue.to_string(), ItemKind::Issue, 5).is_err());

        let pull = json!({"number": 7, "title": "Fix", "body": "", "state": "MERGED",
            "author": null, "labels": [], "createdAt": "2026-01-01T00:00:00Z",
            "headRefName": "topic", "baseRefName": "main", "isDraft": true});
        let details = github_details(&pull.to_string(), ItemKind::Pr, 7).unwrap();
        assert_eq!(details.state, "merged");
        assert_eq!(details.author, "ghost");
        assert!(details.comments.is_none());
        let pr = details.pr.unwrap();
        assert_eq!(
            (pr.head.as_str(), pr.base.as_str(), pr.draft),
            ("topic", "main", true)
        );
    }

    #[test]
    fn github_inline_comments_join_the_review_that_posted_them() {
        let reviews = [
            json!({"id": 1, "user": {"login": "bot"}, "state": "CHANGES_REQUESTED", "body": "Fix it", "submitted_at": "t1"}),
            json!({"id": 2, "user": {"login": "me"}, "state": "PENDING", "body": "", "submitted_at": null}),
            json!({"id": 3, "user": {"login": "me"}, "state": "COMMENTED", "body": "", "submitted_at": "t2"}),
        ];
        let comments = [
            json!({"pull_request_review_id": 1, "user": {"login": "bot"}, "path": "a.rs", "line": 3, "original_line": 2, "body": "Bug", "created_at": "t1"}),
            json!({"pull_request_review_id": 3, "user": {"login": "me"}, "path": "a.rs", "line": null, "original_line": 2, "body": "Fixed", "created_at": "t2"}),
        ];
        let reviews = github_reviews(&reviews, &comments).unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].state, "changes_requested");
        assert_eq!(reviews[0].comments[0].line, Some(3));
        assert_eq!(reviews[0].comments[0].resolved, None);
        assert_eq!(reviews[1].comments[0].body, "Fixed");
        assert_eq!(reviews[1].comments[0].line, Some(2));
        assert!(github_reviews(&[json!({"id": 4, "state": "LATER"})], &[]).is_err());
    }

    #[test]
    fn forgejo_details_distinguish_merged_prs_and_refuse_prs_as_issues() {
        let pull = json!({"number": 541, "title": "Share", "body": "Why", "state": "closed",
            "merged": true, "user": {"login": "dev"}, "labels": [{"name": "type/chore"}],
            "created_at": "2026-10-10T01:00:00+02:00", "head": {"ref": "topic"}, "base": {"ref": "main"},
            "draft": false, "pull_request": {"merged": true}});
        let details = forgejo_details(&pull, ItemKind::Pr, 541).unwrap();
        assert_eq!(details.state, "merged");
        assert_eq!(details.labels, ["type/chore"]);
        assert_eq!(details.pr.unwrap().head, "topic");
        assert!(forgejo_details(&pull, ItemKind::Issue, 541).is_err());

        let issue = json!({"number": 548, "title": "View", "body": "Text", "state": "open",
            "user": {"login": "HN05"}, "labels": [], "created_at": "t", "pull_request": null});
        let details = forgejo_details(&issue, ItemKind::Issue, 548).unwrap();
        assert_eq!(
            (details.state.as_str(), details.body.as_str()),
            ("open", "Text")
        );
    }

    #[test]
    fn forgejo_reviews_carry_inline_comments_and_resolution() {
        let reviews = [
            (
                json!({"id": 1, "user": {"login": "bot"}, "state": "COMMENT", "body": "Blocker", "submitted_at": "t1"}),
                json!([
                    {"user": {"login": "bot"}, "path": "a.py", "position": 182, "original_position": 0,
                        "body": "Fetch first", "created_at": "t1", "resolver": {"login": "dev"}},
                    {"user": {"login": "dev"}, "path": "a.py", "position": 182, "original_position": 0,
                        "body": "Done", "created_at": "t2", "resolver": null},
                    {"user": {"login": "dev"}, "path": "a.py", "position": 0, "original_position": 9,
                        "body": "Old", "created_at": "t3", "resolver": null}
                ]),
            ),
            (
                json!({"id": 2, "user": {"login": "x"}, "state": "REQUEST_REVIEW", "body": ""}),
                json!([]),
            ),
        ];
        let reviews = forgejo_reviews(&reviews).unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].state, "commented");
        let comments = &reviews[0].comments;
        assert_eq!(
            (comments[0].line, comments[0].resolved),
            (Some(182), Some(true))
        );
        assert_eq!(
            (comments[1].line, comments[1].resolved),
            (Some(182), Some(true))
        );
        assert_eq!(
            (comments[2].line, comments[2].resolved),
            (Some(9), Some(false))
        );
        assert_eq!(
            forgejo_comments(&json!([{"user": {"login": "a"}, "body": "b", "created_at": "c"}]))
                .unwrap()[0]
                .author,
            "a"
        );
    }

    #[test]
    fn failed_views_omit_details_and_round_trip() {
        let view = ItemView {
            url: "https://forge.example/o/r/issues/1".into(),
            kind: ItemKind::Issue,
            details: None,
            errors: vec!["lookup failed".into()],
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert_eq!(serde_json::from_value::<ItemView>(json).unwrap(), view);
    }
}
