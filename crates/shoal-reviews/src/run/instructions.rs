//! What a review's reviewer is told: the review instructions, then the history
//! of earlier reviews and the PR's discussion.
use anyhow::Result;
use shoal_forge::activity::view::ViewRequests;
use shoal_refs::BranchName;

use super::attempts::InPlace;
use crate::{
    Reviews, config::templates, daemon::Daemon, findings, history, log, model::Workspace,
    reviews::ReviewTarget, store,
};

impl Reviews {
    /// The review's instructions for the checkout `place` holds, then its
    /// history.
    pub(super) async fn review_instructions(
        &self,
        daemon: &dyn Daemon,
        place: &InPlace<'_>,
    ) -> Result<String> {
        let InPlace {
            review,
            workspace,
            settings,
            commits,
            held,
            ..
        } = place;
        let checkout = held.files.checkout();
        let instructions = templates::review_instructions(
            (settings.rules.get(crate::config::modes::REVIEW_RULE)).map(|rule| rule.text.as_str()),
            &templates::ReviewFields {
                workspace: workspace.directory_name(),
                branch: workspace
                    .branch()
                    .map(BranchName::as_str)
                    .unwrap_or_default(),
                path: &checkout.to_string_lossy(),
                base: commits.base.as_str(),
            },
        );
        let (id, workspace_id) = (review.id, workspace.id.clone());
        let false_positives = findings::listed(daemon, workspace.repository()).await?;
        let records = self.records.clone();
        let earlier = self
            .store
            .run(move |db| store::earlier(db, &*records, &workspace_id, id))
            .await?;
        let pull = match &review.target {
            ReviewTarget::Pr { url, .. } => pull_discussion(daemon, workspace, url).await,
            ReviewTarget::Workspace => None,
        };
        // The history fills what the instructions leave of the prompt.
        let room = history::PROMPT_LIMIT.saturating_sub(instructions.len() + 2);
        let history = history::history(&false_positives, &earlier, pull.as_ref(), room);
        Ok(match history.is_empty() {
            true => instructions,
            false => format!("{instructions}\n\n{history}"),
        })
    }
}

/// The PR's description, conversation and reviews as its agent account
/// reads them; a review without them still runs.
pub(super) async fn pull_discussion(
    daemon: &dyn Daemon,
    workspace: &Workspace,
    url: &crate::forge::items::reference::ItemRef,
) -> Option<crate::forge::activity::view::Details> {
    let (forge, account) = match daemon.forge_account(workspace).await {
        Ok(found) => found,
        Err(error) => {
            log!("the discussion of {url}: {error:#}");
            return None;
        }
    };
    let view = forge
        .view(&account, crate::forge::items::link::ItemKind::Pr, url, true)
        .await;
    for error in &view.errors {
        log!("the discussion of {url}: {error}");
    }
    view.details
}
