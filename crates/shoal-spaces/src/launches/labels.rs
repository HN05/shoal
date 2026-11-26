//! The mode a launch that names none takes from the labels of the issue or
//! PR it names, with the item read from the forge as its caller reads it.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{LabelledMode, Launch};

use crate::scope::Caller;
use shoal_core::launches::{self, labels};

impl Spaces {
    /// Names the spaces the launch names by ID first, for a client that
    /// settles the rest of the start.
    pub async fn launch_labelled_mode(
        &self,
        daemon: &dyn Daemon,
        mut launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<LabelledMode> {
        self.select_launch_spaces(daemon, &mut launch).await?;
        self.labelled_launch_mode(daemon, &launch, caller).await
    }

    /// The labels of the PR, else of the issue, choose among the
    /// [`labels::labelled`] modes it may start in. Unlabelled without such an
    /// item, a mode with a label it may take, or a registered repository to
    /// read the item in.
    pub async fn labelled_launch_mode(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<LabelledMode> {
        if !labels::names_item(launch)? {
            return Ok(LabelledMode::Unlabelled);
        }
        let modes = self.launch_modes(daemon, launch, caller).await?;
        let labelled = labels::labelled(launch, &modes)?;
        if labelled.is_empty() {
            return Ok(LabelledMode::Unlabelled);
        }
        let Some(repository) = self.checked_repository(daemon, launch, caller).await? else {
            return Ok(LabelledMode::Unlabelled);
        };
        Ok(
            match self
                .launch_item_labels(daemon, launch, &repository, caller)
                .await?
            {
                Some((item, carried)) => labels::labelled_mode(item, labelled, &carried),
                None => LabelledMode::Unlabelled,
            },
        )
    }

    /// The PR it names, else the issue, as refusals name it, read as
    /// `caller` reads it. None where it names neither.
    async fn launch_item_labels(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<Option<(String, Vec<String>)>> {
        let context = self.launch_read_context(repository, caller).await?;
        if let Some(pull) = launches::named_pull(launch) {
            let pull = (daemon.pull_request(&context, &pull, caller)).await?;
            return Ok(Some((format!("PR {}", pull.url), pull.labels)));
        }
        let Some(issue) = launches::named_issue(launch)? else {
            return Ok(None);
        };
        let read = daemon.issue_details(&context, &issue, false, caller);
        Ok(Some((format!("issue {issue}"), read.await?.labels)))
    }
}
