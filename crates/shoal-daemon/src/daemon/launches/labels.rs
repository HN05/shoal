//! The mode a launch that names none takes from the labels of the issue or
//! PR it names, as [`launches::labels`] decides it among the modes it may
//! start in, with the item read from the forge as its caller reads it.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{LabelledMode, Launch};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    launches::{self, labels},
};

impl Manager {
    /// The mode the labels of the issue or PR `launch` names call for, as
    /// [`Self::labelled_launch_mode`] decides it, once the spaces it names
    /// are named by ID, for a client that settles the rest of the start.
    pub(crate) async fn launch_labelled_mode(
        &self,
        mut launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<LabelledMode> {
        self.select_launch_spaces(&mut launch).await?;
        self.labelled_launch_mode(&launch, caller).await
    }

    /// The mode the labels of the PR `launch` names, else of the issue,
    /// call for among the [`labels::labelled`] modes it may start in for
    /// `caller`; unlabelled without such an item, a mode with a label it
    /// may take or a registered repository to read the item in.
    pub(super) async fn labelled_launch_mode(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<LabelledMode> {
        if !labels::names_item(launch)? {
            return Ok(LabelledMode::Unlabelled);
        }
        let modes = self.launch_modes(launch, caller).await?;
        let labelled = labels::labelled(launch, &modes)?;
        if labelled.is_empty() {
            return Ok(LabelledMode::Unlabelled);
        }
        let Some(repository) = self.checked_repository(launch, caller).await? else {
            return Ok(LabelledMode::Unlabelled);
        };
        Ok(
            match self.launch_item_labels(launch, &repository, caller).await? {
                Some((item, carried)) => labels::labelled_mode(item, labelled, &carried),
                None => LabelledMode::Unlabelled,
            },
        )
    }

    /// What `launch` starts on in `repository`, the PR it names, else the
    /// issue, as refusals name it, with its labels, read as `caller` reads
    /// it; none where it names neither.
    async fn launch_item_labels(
        &self,
        launch: &Launch,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<Option<(String, Vec<String>)>> {
        let context = self.launch_read_context(repository, caller).await?;
        if let Some(pull) = launches::named_pull(launch) {
            let pull = (self.links.pull_request(self, &context, &pull, caller)).await?;
            return Ok(Some((format!("PR {}", pull.url), pull.labels)));
        }
        let Some(issue) = launches::named_issue(launch)? else {
            return Ok(None);
        };
        let read = self
            .links
            .issue_details(self, &context, &issue, false, caller);
        Ok(Some((format!("issue {issue}"), read.await?.labels)))
    }
}

#[cfg(test)]
mod tests;
