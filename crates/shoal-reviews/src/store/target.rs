//! A review's target as its row keeps it: a PR's URL in the `target_url`
//! column, which joins the reviews of one subject, and the rest as JSON.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use shoal_refs::BranchName;

use crate::{forge::items::reference::ItemRef, reviews::ReviewTarget};

/// What the `target` column holds of a [`ReviewTarget`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Stored {
    Workspace,
    Pr {
        head: BranchName,
        base: BranchName,
        post: bool,
    },
}

/// The `target_url` and `target` columns of `target`.
pub(super) fn columns(target: &ReviewTarget) -> Result<(Option<&ItemRef>, String)> {
    let (url, stored) = match target {
        ReviewTarget::Workspace => (None, Stored::Workspace),
        ReviewTarget::Pr {
            url,
            head,
            base,
            post,
        } => (
            Some(url),
            Stored::Pr {
                head: head.clone(),
                base: base.clone(),
                post: *post,
            },
        ),
    };
    Ok((url, serde_json::to_string(&stored)?))
}

/// The target the `target_url` and `target` columns hold.
pub(super) fn read(url: Option<ItemRef>, target: &str) -> Result<ReviewTarget> {
    Ok(match (serde_json::from_str(target)?, url) {
        (Stored::Workspace, None) => ReviewTarget::Workspace,
        (Stored::Pr { head, base, post }, Some(url)) => ReviewTarget::Pr {
            url,
            head,
            base,
            post,
        },
        (Stored::Workspace, Some(url)) => bail!("a review of the workspace names the PR {url}"),
        (Stored::Pr { .. }, None) => bail!("a review of a PR names no PR"),
    })
}

#[cfg(test)]
mod tests;
