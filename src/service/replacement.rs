//! Refuse state-directory changes while the existing service can still run.
use super::{
    LABEL, Platform, UNIT,
    control::{command, domain, query},
    definition::state_dir_matches,
};
use crate::{paths::Paths, tools::Tool};
use anyhow::{Result, ensure};
use std::path::Path;

pub(super) async fn check(paths: &Paths, definition: &Path, platform: Platform) -> Result<()> {
    if !definition.exists() || state_dir_matches(paths, definition, platform)? {
        return Ok(());
    }
    ensure!(
        stopped(platform).await?,
        "installed service uses another state directory; stop the existing service with `shoal --state-dir <existing-directory> daemon stop` before rerunning `shoal daemon install`"
    );
    Ok(())
}

async fn stopped(platform: Platform) -> Result<bool> {
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            Ok(!command(Tool::Launchctl, &["print", &target], false).await?)
        }
        Platform::Linux => {
            let state = query(
                Tool::Systemctl,
                &["--user", "show", "--property=ActiveState", "--value", UNIT],
            )
            .await?;
            Ok(state.trim() == "inactive")
        }
    }
}
