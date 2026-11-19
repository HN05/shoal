//! Whether a launch of a new workspace starts an agent there, as a start
//! without a terminal to pick one chooses.
use anyhow::{Result, ensure};
use shoal_protocol::launches::Launch;

use super::target::Target;
use crate::daemon::workspace::Manager;

impl Manager {
    /// Whether `launch` starts an agent on `target`: the one it names, else
    /// for a start on a positional issue `agent.default`, else none, as no
    /// terminal picks one.
    pub(super) async fn planned_agent(&self, launch: &Launch, target: &Target) -> Result<bool> {
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
