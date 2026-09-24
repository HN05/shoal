//! Worktree creation and removal through Worktrunk (`wt`).
use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::{
    removal::{BranchOutcome, RemovalResult},
    subprocess,
};

#[cfg(test)]
mod tests;

/// Fields consumed from Worktrunk 0.78.0's SwitchJsonOutput. Other fields are
/// informational; only `created` (not `existing` or `already_at`) is success.
#[derive(Debug, Deserialize)]
struct CreationOutput {
    action: String,
    path: PathBuf,
}

/// Fields consumed from RemovalPlan::to_json in Worktrunk 0.78.0. A detached
/// worktree has no branch. Extra fields vary for worktree/branch-only removal.
#[derive(Deserialize)]
struct RemovalOutput {
    branch: Option<String>,
    branch_outcome: BranchOutcome,
}

fn decode_object<T: DeserializeOwned>(value: Value) -> Result<T> {
    // Serde structs can also accept positional arrays; the external contract
    // requires objects, including after singleton-array normalization.
    ensure!(value.is_object(), "expected a Worktrunk result object");
    Ok(serde_json::from_value(value)?)
}

fn decode_creation(output: &str, workspace_dir: &Path) -> Result<()> {
    let value = serde_json::from_str(output).context("invalid Worktrunk creation result")?;
    let result: CreationOutput =
        decode_object(value).context("invalid Worktrunk creation result")?;
    ensure!(
        result.action == "created",
        "Worktrunk did not create a new workspace: {result:?}"
    );
    ensure!(
        std::fs::canonicalize(result.path)? == std::fs::canonicalize(workspace_dir)?,
        "Worktrunk created an unexpected workspace path"
    );
    Ok(())
}

fn decode_removal(output: &str, workspace_dir: &Path) -> Result<RemovalResult> {
    let result = match serde_json::from_str(output).context("invalid Worktrunk removal result")? {
        Value::Array(mut entries) => {
            ensure!(
                entries.len() == 1,
                "unexpected number of Worktrunk removal results"
            );
            entries.remove(0)
        }
        result => result,
    };
    let result: RemovalOutput =
        decode_object(result).context("invalid Worktrunk removal result")?;
    ensure!(
        !workspace_dir.exists(),
        "Worktrunk returned before workspace removal completed"
    );
    Ok(RemovalResult {
        removed: true,
        branch: result.branch,
        branch_outcome: result.branch_outcome,
        hook_error: None,
    })
}

/// Names that cannot portably identify a literal branch through Worktrunk.
/// Worktrunk 0.78.0 expands `@`; Git treats `HEAD` and full object IDs
/// specially. Reserve both supported object-ID lengths regardless of the
/// repository's format. Check the whole name, not individual components:
/// `topic/HEAD` and `HEAD/topic` are literal branches.
pub fn is_reserved_branch_name(name: &str) -> bool {
    matches!(name, "HEAD" | "@")
        || (matches!(name.len(), 40 | 64) && name.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn command(repository_dir: &Path, worktrunk_config: &Path) -> Command {
    let mut command = Command::new(Tool::Worktrunk.program());
    command.arg("--config").arg(worktrunk_config);
    command.arg("-C").arg(repository_dir);
    command
}

pub async fn create(
    repository_dir: &Path,
    worktrunk_config: &Path,
    workspace_dir: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<()> {
    let literal = serde_json::to_string(
        workspace_dir
            .to_str()
            .context("workspace path is not UTF-8")?,
    )?;
    // Worktrunk renders this setting as a template. Emit the entire path as
    // one string expression so template syntax in a literal directory stays inert.
    let template = serde_json::to_string(&format!("{{{{ {literal} }}}}"))?;
    let mut command = Command::new(Tool::Worktrunk.program());
    command
        .arg("--config")
        .arg(worktrunk_config)
        .arg("--config-set")
        .arg(format!("worktree-path = {template}"))
        .arg("-C")
        .arg(repository_dir)
        .arg("switch");
    if let Some(base) = base {
        command.args(["--create", "--base", base]);
    } else {
        // Worktrunk otherwise routes the default branch to the main checkout,
        // ignoring worktree-path. Use a full ref for this invocation only:
        // it still resolves, but cannot equal a literal branch name.
        default_branch_override(&mut command, &crate::git::local_ref(branch))?;
    }
    command.args([branch, "--no-cd", "--no-hooks", "--format=json"]);
    decode_creation(&subprocess::output(command).await?, workspace_dir)
}

fn default_branch_override(command: &mut Command, reference: &str) -> Result<()> {
    let count: usize = std::env::var("GIT_CONFIG_COUNT")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .context("invalid GIT_CONFIG_COUNT")?;
    command
        .env("GIT_CONFIG_COUNT", (count + 1).to_string())
        .env(
            format!("GIT_CONFIG_KEY_{count}"),
            "worktrunk.default-branch",
        )
        .env(format!("GIT_CONFIG_VALUE_{count}"), reference);
    Ok(())
}

pub async fn remove(
    repository_dir: &Path,
    worktrunk_config: &Path,
    workspace_dir: &Path,
    force_files: bool,
    delete_branch: bool,
) -> Result<RemovalResult> {
    let mut command = command(repository_dir, worktrunk_config);
    // Shoal has verified ownership and made the branch-retention decision.
    // Worktrunk otherwise refuses even --no-delete-branch for the default branch.
    default_branch_override(&mut command, "HEAD")?;
    command.args(["remove", "--foreground", "--no-hooks", "--format=json"]);
    command.arg(if delete_branch {
        "--force-delete"
    } else {
        "--no-delete-branch"
    });
    if force_files {
        command.arg("--force");
    }
    command.arg("--").arg(workspace_dir);
    decode_removal(&subprocess::output(command).await?, workspace_dir)
}
