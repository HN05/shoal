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
            id: comment["id"].as_u64().unwrap_or_default(),
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
mod tests;
