//! The name of a new space, as every start gives it, whoever sends it: the
//! one the start names, else its issue's title, else the title of the PR a
//! workspace opens at its head or links, else its home's.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_protocol::space_names::SpaceName;

use super::{Preparing, StartIssue, home::Home};
use crate::{config::modes::ModeName, log, protocol::operations::WorkspaceStart};

impl Spaces {
    /// The name of the space at `home` that a start `named`, on `issue`,
    /// with what `preparing` says follows; `None` for a workspace named
    /// after its branch.
    pub(super) async fn start_name(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        named: Option<SpaceName>,
        issue: Option<&StartIssue>,
        preparing: &Preparing,
    ) -> Result<Option<SpaceName>> {
        if named.is_some() {
            return Ok(named);
        }
        let fallback = home.kind().to_string();
        if let Some(title) = issue.and_then(|issue| issue.title.as_ref()) {
            return Ok(Some(SpaceName::derive(title, &fallback)));
        }
        if let Some(title) = self.opened_pull_title(daemon, home, preparing).await {
            return Ok(Some(SpaceName::derive(&title, &fallback)));
        }
        let mode = preparing.mode().unwrap_or(ModeName::DEFAULT);
        self.home_name(daemon, home, &mode).await
    }

    /// The title of the PR a workspace a start opens opens at its head or
    /// links; a PR that cannot be read leaves it named after its branch,
    /// and linking it reports why.
    async fn opened_pull_title(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        preparing: &Preparing,
    ) -> Option<String> {
        let Home::Opening(repository, opening) = home else {
            return None;
        };
        let input = match &opening.start {
            WorkspaceStart::PullHead { url, .. } => &url.clone().into(),
            _ => preparing.pr.as_ref()?,
        };
        let read = async {
            let checkout = daemon.repository(repository.clone()).await?.path;
            daemon.read_pull_title(&checkout, input).await
        };
        match read.await {
            Ok(title) => Some(title),
            Err(error) => {
                log!("PR {input} not read to name its workspace: {error:#}");
                None
            }
        }
    }
}
