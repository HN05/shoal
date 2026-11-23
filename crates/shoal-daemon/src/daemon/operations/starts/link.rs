//! Linking a new space to the issue its start names, assigned, as the
//! caller's account on the forge: what that warns of reaches the space's
//! starter, and a new space whose issue cannot be linked is removed, since
//! nothing ran in it yet.
use anyhow::Result;
use shoal_links::issue::assignees::AssignerOf;
use shoal_links::reads::checkouts::OpenIssue;
use shoal_protocol::space_names::SpaceSelector;

use super::home::Recorded;
use crate::{
    daemon::{log, manager::Manager, notifications::AlertKind, scope::Caller},
    forge::items::reference::ItemRef,
    model::{Assigner, Space},
    protocol::operations::LaunchIssue,
    removal::{InspectionPolicy, UnsavedWork},
};

/// The issue a new space starts on, by its URL, with the title and labels
/// it was read open with, unless a launch that names the space could not
/// read it.
pub(crate) struct StartIssue {
    pub url: ItemRef,
    pub title: Option<String>,
    pub labels: Vec<String>,
}

impl From<OpenIssue> for StartIssue {
    fn from(OpenIssue { url, title, labels }: OpenIssue) -> Self {
        Self {
            url,
            title: Some(title),
            labels,
        }
    }
}

impl From<&StartIssue> for LaunchIssue {
    /// The issue an agent works on, titled by its URL where it was not read.
    fn from(issue: &StartIssue) -> Self {
        Self {
            url: issue.url.clone(),
            title: (issue.title.clone()).unwrap_or_else(|| issue.url.to_string()),
            labels: issue.labels.clone(),
        }
    }
}

impl Manager {
    /// Assign `issue` to the `recorded` space, as `caller`'s account on
    /// the forge, telling its starter what that warns of, which it returns.
    /// A new space whose issue cannot be assigned is removed.
    pub(super) async fn link_start_issue(
        &self,
        recorded: &Recorded,
        issue: Option<StartIssue>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let Some(StartIssue { url, title, .. }) = issue else {
            return Ok(Vec::new());
        };
        let space = &recorded.space;
        let selector = SpaceSelector::id(&space.id);
        let assigned = self
            .links
            .set_issue(self, &selector, &url.into(), title, Assigner::of(caller))
            .await;
        if assigned.is_err() && !recorded.reused {
            self.discard_new(space).await;
        }
        let warnings = assigned?;
        self.tell_start_warnings(space, &warnings).await;
        Ok(warnings)
    }

    /// Tell the starter of the new `space` what starting it warned of: the
    /// agent of the nearest space above it with one, else the user.
    pub(in crate::daemon) async fn tell_start_warnings(&self, space: &Space, warnings: &[String]) {
        if warnings.is_empty() {
            return;
        }
        let message = format!("starting {} warned: {}", space.name, warnings.join("; "));
        let kind = AlertKind::StartWarned;
        self.coordination
            .notify_space(self, &space.name, &space.id, kind, message)
            .await;
    }

    /// Remove the new `space`, which nothing ran in yet.
    async fn discard_new(&self, space: &Space) {
        let removal = self
            .remove_selected(
                &SpaceSelector::id(&space.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
            .await;
        if let Err(error) = removal {
            log!(
                "cannot remove {} after its creation failed: {error:#}",
                space.name
            );
        }
    }
}
