//! Git bundles that carry a workspace's checkpoint to another repository.
use std::path::Path;

use anyhow::{Context, Result};

use super::FETCH_SAFE_ARGS;
use crate::subprocess;

/// Write the objects `reference` reaches and `exclude` does not to `file`,
/// under the same ref.
pub async fn create(
    repo: &Path,
    file: &Path,
    reference: &str,
    exclude: Option<&str>,
) -> Result<()> {
    let mut command = super::isolated_command(repo);
    command
        .args(["bundle", "create", "--quiet"])
        .arg(file)
        .arg(reference);
    if let Some(exclude) = exclude {
        command.arg(format!("^{exclude}"));
    }
    subprocess::output(command).await?;
    Ok(())
}

/// Fetch `reference` from the bundle `file` into the same ref of `repo`; Git
/// refuses a bundle whose excluded commits the repository lacks.
pub async fn fetch(repo: &Path, file: &Path, reference: &str) -> Result<()> {
    let file = file.to_str().context("bundle path is not UTF-8")?;
    let refspec = format!("{reference}:{reference}");
    super::run_isolated(repo, &[FETCH_SAFE_ARGS, &["--", file, &refspec]].concat()).await?;
    Ok(())
}
