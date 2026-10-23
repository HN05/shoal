//! Forgejo items as its REST API reports them, for item views.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::threads::{review_comments, thread_key, threads};
use crate::{
    ForgeRepo,
    account::Account,
    items::link::ItemKind,
    review::view::{
        Comment, Details, PrDetails, Review, ReviewComment, labels, login, required,
        review_comment, review_state, text_field,
    },
    transport::api::Request,
};

pub(crate) async fn details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Details> {
    let item = repo
        .send(account, &Request::get(repo.item_endpoint(kind, number)))
        .await?;
    parse_details(&item, kind, number)
}

/// Fills the item's comments and returns a PR's reviews, recording in
/// `errors` each review whose inline comments could not be read.
pub(crate) async fn discussion(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    details: &mut Details,
    errors: &mut Vec<String>,
) -> Result<Option<Vec<Review>>> {
    let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
    let comments = repo.send(account, &Request::get(endpoint)).await?;
    details.comments = Some(parse_comments(&comments)?);
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    parse_reviews(&review_comments(repo, account, number).await?, errors).map(Some)
}

fn parse_details(item: &Value, kind: ItemKind, number: u64) -> Result<Details> {
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
        ItemKind::Pr => Some(PrDetails::read(
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

fn parse_comments(comments: &Value) -> Result<Vec<Comment>> {
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
/// included, each in its review's thread on the same lines. A review whose
/// comments could not be read keeps its body, and `errors` names it.
fn parse_reviews(
    reviews: &[(Value, Result<Value>)],
    errors: &mut Vec<String>,
) -> Result<Vec<Review>> {
    let mut result = Vec::new();
    for (review, comments) in reviews {
        let Some(state) = review_state(review["state"].as_str())? else {
            continue;
        };
        let id = review["id"].as_u64().unwrap_or_default();
        let comments = match comments {
            Ok(comments) => parse_review_comments(id, comments)?,
            Err(error) => {
                errors.push(format!("comments lookup failed for review {id}: {error:#}"));
                Vec::new()
            }
        };
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

/// The inline comments of review `review`, each in its thread.
fn parse_review_comments(review: u64, comments: &Value) -> Result<Vec<ReviewComment>> {
    let comments: Vec<(u64, &Value)> = comments
        .as_array()
        .context("Forgejo review comments are not a list")?
        .iter()
        .map(|comment| (review, comment))
        .collect();
    let threads = threads(&comments);
    Ok(comments
        .iter()
        .map(|&(review, comment)| {
            let id = comment["id"].as_u64().unwrap_or_default();
            let (thread, resolved) = threads
                .get(&thread_key(review, comment))
                .copied()
                .unwrap_or((id, false));
            review_comment(
                comment,
                &comment["position"],
                &comment["original_position"],
                thread,
                Some(resolved),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use serde_json::json;

    #[test]
    fn forgejo_details_distinguish_merged_prs_and_refuse_prs_as_issues() {
        let pull = json!({"number": 541, "title": "Share", "body": "Why", "state": "closed",
            "merged": true, "user": {"login": "dev"}, "labels": [{"name": "type/chore"}],
            "created_at": "2026-10-10T01:00:00+02:00", "head": {"ref": "topic"}, "base": {"ref": "main"},
            "draft": false, "pull_request": {"merged": true}});
        let details = parse_details(&pull, ItemKind::Pr, 541).unwrap();
        assert_eq!(details.state, "merged");
        assert_eq!(details.labels, ["type/chore"]);
        assert_eq!(details.pr.unwrap().head, "topic");
        assert!(parse_details(&pull, ItemKind::Issue, 541).is_err());

        let issue = json!({"number": 548, "title": "View", "body": "Text", "state": "open",
            "user": {"login": "HN05"}, "labels": [], "created_at": "t", "pull_request": null});
        let details = parse_details(&issue, ItemKind::Issue, 548).unwrap();
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
                Ok(json!([
                    {"id": 20, "user": {"login": "bot"}, "path": "a.py", "position": 182, "original_position": 0,
                        "body": "Fetch first", "created_at": "t1", "resolver": {"login": "dev"}},
                    {"id": 21, "user": {"login": "dev"}, "path": "a.py", "position": 182, "original_position": 0,
                        "body": "Done", "created_at": "t2", "resolver": null},
                    {"id": 22, "user": {"login": "dev"}, "path": "a.py", "position": 0, "original_position": 9,
                        "body": "Old", "created_at": "t3", "resolver": null}
                ])),
            ),
            (
                json!({"id": 2, "user": {"login": "x"}, "state": "REQUEST_REVIEW", "body": ""}),
                Ok(json!([])),
            ),
            (
                json!({"id": 3, "user": {"login": "dev"}, "state": "COMMENT", "body": "", "submitted_at": "t4"}),
                Ok(json!([
                    {"id": 23, "user": {"login": "dev"}, "path": "a.py", "position": 0, "original_position": 9,
                        "body": "Reply", "created_at": "t4", "resolver": null}
                ])),
            ),
        ];
        let mut errors = Vec::new();
        let reviews = parse_reviews(&reviews, &mut errors).unwrap();
        assert!(errors.is_empty());
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].state, "commented");
        let comments = &reviews[0].comments;
        let thread =
            |comment: &ReviewComment| (comment.id, comment.thread, comment.line, comment.resolved);
        assert_eq!(thread(&comments[0]), (20, 20, Some(182), Some(true)));
        assert_eq!(thread(&comments[1]), (21, 20, Some(182), Some(true)));
        assert_eq!(thread(&comments[2]), (22, 22, Some(9), Some(false)));
        // Another review's comment on the same line starts its own thread.
        assert_eq!(
            thread(&reviews[1].comments[0]),
            (23, 23, Some(9), Some(false))
        );
        assert_eq!(
            parse_comments(&json!([{"user": {"login": "a"}, "body": "b", "created_at": "c"}]))
                .unwrap()[0]
                .author,
            "a"
        );
    }

    #[test]
    fn a_review_whose_comments_fail_keeps_its_body_and_the_others() {
        let reviews = [
            (
                json!({"id": 4, "user": {"login": "bot"}, "state": "COMMENT", "body": "Findings", "submitted_at": "t1"}),
                Err(anyhow!(
                    "GET pulls/7/reviews/4/comments: connection refused"
                )),
            ),
            (
                json!({"id": 5, "user": {"login": "bot"}, "state": "APPROVED", "body": "Fine", "submitted_at": "t2"}),
                Ok(
                    json!([{"id": 30, "user": {"login": "bot"}, "path": "a.py", "position": 1,
                    "original_position": 0, "body": "Nit", "created_at": "t2", "resolver": null}]),
                ),
            ),
        ];
        let mut errors = Vec::new();
        let reviews = parse_reviews(&reviews, &mut errors).unwrap();
        assert_eq!(
            errors,
            [
                "comments lookup failed for review 4: GET pulls/7/reviews/4/comments: connection refused"
            ]
        );
        assert_eq!(
            (reviews[0].body.as_str(), reviews[0].comments.len()),
            ("Findings", 0)
        );
        assert_eq!(reviews[1].comments[0].id, 30);
    }
}
