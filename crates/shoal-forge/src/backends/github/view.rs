//! GitHub items as `gh` and its REST API report them, for item views.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    Query, gh,
    threads::{GitHubThread, threads},
};
use crate::{
    ForgeRepo,
    account::Account,
    items::link::ItemKind,
    review::view::{
        Comment, Details, PrDetails, Review, labels, login, required, review_comment, review_state,
        text_field,
    },
};

pub(crate) async fn details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    comments: bool,
) -> Result<Details> {
    let id = number.to_string();
    let repository = format!("{}/{}", repo.host, repo.path);
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
    let output = gh(account, &args, Query::View).await?;
    parse_details(&output, kind, number)
}

/// Reviews with their inline comments; issues have none.
pub(crate) async fn reviews(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Option<Vec<Review>>> {
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let pages = |resource: &'static str| {
        let endpoint = format!("repos/{}/pulls/{number}/{resource}", repo.path);
        async move {
            let args = [
                "api",
                "--hostname",
                &repo.host,
                &endpoint,
                "--paginate",
                "--slurp",
            ];
            let output = gh(account, &args, Query::View).await?;
            let pages: Vec<Vec<Value>> = serde_json::from_str(&output)
                .with_context(|| format!("invalid gh {resource} response"))?;
            Ok::<_, anyhow::Error>(pages.into_iter().flatten().collect::<Vec<_>>())
        }
    };
    let (reviews, comments, threads) = tokio::try_join!(
        pages("reviews"),
        pages("comments"),
        threads(repo, account, number)
    )?;
    parse_reviews(&reviews, &comments, &threads).map(Some)
}

fn parse_details(text: &str, kind: ItemKind, number: u64) -> Result<Details> {
    let item: Value = serde_json::from_str(text).context("invalid gh response")?;
    ensure!(item["number"] == number, "gh returned a different item");
    let pr = match kind {
        ItemKind::Issue => None,
        ItemKind::Pr => Some(PrDetails::read(
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
/// posted it, including replies, and takes its thread's resolution.
fn parse_reviews(
    reviews: &[Value],
    comments: &[Value],
    threads: &[GitHubThread],
) -> Result<Vec<Review>> {
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
                    let id = comment["id"].as_u64().unwrap_or_default();
                    let thread = threads.iter().find(|thread| thread.comments.contains(&id));
                    review_comment(
                        comment,
                        &comment["line"],
                        &comment["original_line"],
                        comment["in_reply_to_id"].as_u64().unwrap_or(id),
                        thread.map(|thread| thread.resolved),
                    )
                })
                .collect(),
        });
    }
    Ok(result)
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
        let details = parse_details(&issue.to_string(), ItemKind::Issue, 4).unwrap();
        assert_eq!(details.title, "Crash");
        assert_eq!(details.state, "open");
        assert_eq!(details.author, "ann");
        assert_eq!(details.labels, ["bug"]);
        assert!(details.pr.is_none());
        assert_eq!(details.comments.unwrap()[0].body, "Same here");
        assert!(parse_details(&issue.to_string(), ItemKind::Issue, 5).is_err());

        let pull = json!({"number": 7, "title": "Fix", "body": "", "state": "MERGED",
            "author": null, "labels": [], "createdAt": "2026-01-01T00:00:00Z",
            "headRefName": "topic", "baseRefName": "main", "isDraft": true});
        let details = parse_details(&pull.to_string(), ItemKind::Pr, 7).unwrap();
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
            json!({"id": 10, "pull_request_review_id": 1, "user": {"login": "bot"}, "path": "a.rs", "line": 3, "original_line": 2, "body": "Bug", "created_at": "t1"}),
            json!({"id": 11, "in_reply_to_id": 10, "pull_request_review_id": 3, "user": {"login": "me"}, "path": "a.rs", "line": null, "original_line": 2, "body": "Fixed", "created_at": "t2"}),
            json!({"id": 12, "pull_request_review_id": 3, "user": {"login": "me"}, "path": "b.rs", "line": 1, "body": "New", "created_at": "t2"}),
        ];
        let threads = [GitHubThread {
            node: "T_1".into(),
            resolved: true,
            comments: vec![10, 11],
        }];
        let reviews = parse_reviews(&reviews, &comments, &threads).unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].state, "changes_requested");
        let bug = &reviews[0].comments[0];
        assert_eq!((bug.id, bug.thread, bug.line), (10, 10, Some(3)));
        assert_eq!(bug.resolved, Some(true));
        let fixed = &reviews[1].comments[0];
        assert_eq!((fixed.body.as_str(), fixed.line), ("Fixed", Some(2)));
        assert_eq!(
            (fixed.id, fixed.thread, fixed.resolved),
            (11, 10, Some(true))
        );
        // A thread GraphQL did not list has unknown resolution.
        assert_eq!(reviews[1].comments[1].resolved, None);
        assert!(parse_reviews(&[json!({"id": 4, "state": "LATER"})], &[], &[]).is_err());
    }
}
