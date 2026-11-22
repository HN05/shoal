//! An issue's or PR's content, status and discussion, read on request so
//! callers need not know which forge serves it.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    backends::{Activity, Merging},
    capability::CapabilityRequests,
    items::reaction::ReactionRequests,
    review::status::StatusRequests,
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    items::link::ItemKind,
    numbers::{CommentId, PullNumber},
    review::view::{Details, ItemView, ReviewComment},
};

/// Reading items as views.
#[shoal_forge_derive::extension(pub ViewRequests, pub(crate) ViewSteps)]
impl ForgeRepo {
    /// The item at `url` with its discussion when `comments` is set; PRs also
    /// carry the state `shoal status` reports.
    pub async fn view(
        &self,
        account: &Account,
        kind: ItemKind,
        item: &shoal_forge_types::items::reference::ItemRef,
        comments: bool,
    ) -> ItemView {
        let mut errors = Vec::new();
        let details = match self
            .details(account, kind, item, comments, &mut errors)
            .await
        {
            Ok(details) => Some(details),
            Err(error) => {
                errors.insert(0, format!("{error:#}"));
                None
            }
        };
        ItemView {
            url: item.clone(),
            kind,
            details,
            check_logs: Vec::new(),
            errors,
        }
    }

    async fn details(
        &self,
        account: &Account,
        kind: ItemKind,
        item: &shoal_forge_types::items::reference::ItemRef,
        comments: bool,
        errors: &mut Vec<String>,
    ) -> Result<Details> {
        item.belongs_to(self)?;
        let number = item.number().get();
        let forge = self.kind();
        let content = forge.view_details(self, account, kind, number, comments);
        let status = async {
            match kind {
                ItemKind::Pr => Some(tokio::join!(
                    self.pr_status(account, item),
                    forge.behind(self, account, PullNumber::new(number))
                )),
                ItemKind::Issue => None,
            }
        };
        let (content, status) = tokio::join!(content, status);
        let mut details = content?;
        if let (Some(pr), Some((status, behind))) = (&mut details.pr, status) {
            pr.record(status, errors);
            match behind {
                Ok(behind) => pr.behind = behind,
                Err(error) => errors.push(format!("behind lookup failed: {error:#}")),
            }
        }
        if comments {
            let discussion = self
                .kind()
                .view_discussion(self, account, kind, number, &mut details, errors)
                .await;
            match discussion {
                Ok(Some(reviews)) => details.reviews = Some(reviews),
                Ok(None) => {}
                Err(error) => errors.push(format!("comments lookup failed: {error:#}")),
            }
            if self.supports(Capability::Reactions)
                && let Err(error) = self.view_reactions(account, kind, &mut details).await
            {
                errors.push(format!("reactions lookup failed: {error:#}"));
            }
        }
        Ok(details)
    }
}

/// A submitted review's state; pending drafts and review requests are not reviews.
pub(crate) fn review_state(state: Option<&str>) -> Result<Option<String>> {
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
pub(crate) fn review_comment(
    comment: &Value,
    line: &Value,
    original: &Value,
    thread: CommentId,
    resolved: Option<bool>,
) -> ReviewComment {
    let positive = |value: &Value| value.as_u64().filter(|line| *line > 0);
    ReviewComment {
        id: CommentId::new(comment["id"].as_u64().unwrap_or_default()),
        thread,
        author: login(&comment["user"]),
        created_at: text_field(&comment["created_at"]),
        path: text_field(&comment["path"]),
        line: positive(line).or_else(|| positive(original)),
        body: text_field(&comment["body"]),
        resolved,
        reactions: Vec::new(),
    }
}

pub(crate) fn required(value: &Value, field: &str) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("item has no {field}"))
}

pub(crate) fn text_field(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// A deleted account has no user.
pub(crate) fn login(user: &Value) -> String {
    user["login"].as_str().unwrap_or("ghost").to_owned()
}

pub(crate) fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| label["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests;
