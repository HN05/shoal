//! Whether a launch of a new workspace starts an agent there, as a start
//! without a terminal to pick one chooses.
use anyhow::{Result, ensure};
use shoal_protocol::launches::Launch;

use super::target::Target;
use crate::{daemon::workspace::Manager, start_plan::Creation};

impl Manager {
    /// Whether `launch` starts an agent on `target` with `creation`: the
    /// one it names, else for a start on a positional issue
    /// `agent.default`, else none, as no terminal picks one; never one for
    /// a PR from a fork, whose head opens in a workspace at a commit.
    pub(super) async fn planned_agent(
        &self,
        launch: &Launch,
        target: &Target,
        creation: &Creation,
    ) -> Result<bool> {
        if creation.pull_head.is_some() {
            ensure!(
                launch.agent.is_none(),
                "the PR comes from a fork, so its head opens in a workspace at a commit, which holds no agent of its own"
            );
            return Ok(false);
        }
        if launch.agent.is_some() {
            return Ok(true);
        }
        if !target.issue_default {
            ensure!(
                launch.model.is_empty(),
                "--model and --effort need an agent; name one with --agent"
            );
            return Ok(false);
        }
        let settings = self
            .repository_settings_with_templates(&target.repo.id)
            .await?;
        ensure!(
            settings.agent.default.is_some(),
            "no agent selected; pass --agent or set agent.default in the repository or global config"
        );
        Ok(true)
    }
}
