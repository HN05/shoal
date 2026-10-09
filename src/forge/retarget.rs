//! Change a PR's base branch: the one forge write Shoal makes, for stacked
//! workspaces whose base PR merged. GitHub uses `gh pr edit`. fj cannot edit a
//! base, so Forgejo uses its REST API with the token fj saved for that host.
use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use tokio::process::Command;

use super::{
    ForgeKind, ForgeRepo,
    api::{authorization, fj_keys_path, fj_token},
};
use crate::tools::Tool;

const TIMEOUT: Duration = Duration::from_secs(30);

impl ForgeRepo {
    pub(crate) async fn retarget(&self, path: &Path, number: u64, base: &str) -> Result<()> {
        match self.kind {
            ForgeKind::GitHub => self.github_retarget(path, number, base).await,
            ForgeKind::Forgejo => self.forgejo_retarget(number, base).await,
        }
        .with_context(|| format!("could not retarget PR #{number} to {base}"))
    }

    async fn github_retarget(&self, path: &Path, number: u64, base: &str) -> Result<()> {
        let mut gh = Command::new(self.kind.tool());
        let repository = format!("{}/{}", self.host, self.path);
        gh.current_dir(path).env("NO_COLOR", "1").args([
            "pr",
            "edit",
            &number.to_string(),
            "--repo",
            &repository,
            "--base",
            base,
        ]);
        crate::subprocess::Run::new(gh)
            .timeout(TIMEOUT)
            .checked()
            .await?;
        Ok(())
    }

    async fn forgejo_retarget(&self, number: u64, base: &str) -> Result<()> {
        let token = fj_token(&fj_keys_path()?, &self.host)?;
        let url = format!(
            "{}://{}/api/v1/repos/{}/pulls/{number}",
            self.web_scheme, self.host, self.path
        );
        let body = serde_json::json!({ "base": base }).to_string();
        let mut curl = Command::new(Tool::Curl.program());
        // -q first: a user .curlrc must not trace the token header.
        curl.args([
            "-q",
            "-sS",
            "-f",
            "-o",
            "/dev/null",
            "--max-time",
            "30",
            "-X",
            "PATCH",
            "-H",
            "Content-Type: application/json",
            "--data",
            &body,
            "-K",
            "-",
            &url,
        ]);
        crate::subprocess::Run::new(curl)
            .input(authorization(token))
            .timeout(TIMEOUT)
            .checked()
            .await
            .context(
                "Forgejo API request failed; curl and an fj login with write access are required",
            )?;
        Ok(())
    }
}
