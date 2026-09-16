//! Git merges run inside a tracked execution, keeping terminal I/O and child
//! processes in the CLI wrapper. The daemon authorizes the destination workspace.
use std::{os::unix::process::ExitStatusExt, path::Path};

use anyhow::{Context, Result, ensure};
use serde_json::json;
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    client, execution,
    paths::Paths,
    protocol::{Body, Method},
    ui, worktrunk,
};

pub async fn run(
    paths: &Paths,
    workspace: Option<String>,
    branch: String,
    remote: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let mut command = vec![std::env::current_exe()?.into_os_string()];
    if json_output {
        command.push("--json".into());
    }
    command.extend(["merge-internal".into(), branch.into()]);
    if let Some(remote) = remote {
        command.extend(["--remote".into(), remote.into()]);
    }
    execution::run(paths, workspace, command).await
}

pub async fn worker(
    paths: &Paths,
    branch: String,
    remote: Option<String>,
    json_output: bool,
) -> Result<i32> {
    ensure!(
        std::env::var_os("SHOAL_SCOPE_TOKEN").is_some(),
        "merge worker requires a scoped execution"
    );
    let workspace =
        std::env::var("SHOAL_WORKSPACE_ID").context("merge worker requires a workspace")?;
    let Body::Inspection(inspection) = client::call(paths, Method::Inspect { workspace }).await?
    else {
        anyhow::bail!("unexpected workspace response");
    };
    let workspace = inspection.workspace;
    ensure!(
        std::env::current_dir()?.canonicalize()? == workspace.path.canonicalize()?,
        "merge worker must run in its own worktree"
    );
    let path = &workspace.path;
    own_branch(path, &workspace.branch).await?;
    let previous = git(path, &["rev-parse", "HEAD"]).await?;
    let fetched = format!("refs/shoal/merge/{}", Uuid::new_v4());
    let result = async {
        let commit = source(path, &branch, remote.as_deref(), &fetched).await?;
        own_branch(path, &workspace.branch).await?;
        ensure!(
            git(path, &["rev-parse", "HEAD"]).await? == previous,
            "workspace HEAD changed during fetch; retry shoal merge"
        );
        let output = command(path)
            .args([
                "merge",
                "--ff",
                "--no-squash",
                "--no-edit",
                "--no-stat",
                "--no-autostash",
                "--no-overwrite-ignore",
                "-m",
                &format!("Merge branch '{branch}'"),
                "--",
                &commit,
            ])
            .output()
            .await
            .context("merge branch")?;
        let exit_code = output
            .status
            .code()
            .unwrap_or_else(|| 128 + output.status.signal().unwrap_or(1));
        if json_output {
            println!(
                "{}",
                json!({
                    "workspace_id": workspace.id,
                    "source_commit": commit,
                    "previous_commit": previous.trim(),
                    "commit": git(path, &["rev-parse", "HEAD"]).await?.trim(),
                    "success": output.status.success(),
                    "exit_code": exit_code,
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                })
            );
        } else {
            use std::io::Write;
            std::io::stdout().write_all(&output.stdout)?;
            std::io::stderr().write_all(&output.stderr)?;
        }
        Ok(exit_code)
    }
    .await;
    // Never depend on shared FETCH_HEAD or leave a local source branch behind.
    let cleanup = git(path, &["update-ref", "-d", &fetched]).await;
    let code = result?;
    cleanup.context("could not remove temporary merge ref")?;
    Ok(code)
}

async fn own_branch(path: &Path, branch: &str) -> Result<()> {
    ensure!(
        git(path, &["symbolic-ref", "--quiet", "HEAD"]).await? == format!("refs/heads/{branch}\n"),
        "workspace must be on its own recorded branch ({branch}) before merging"
    );
    Ok(())
}

async fn source(path: &Path, branch: &str, remote: Option<&str>, fetched: &str) -> Result<String> {
    let name = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    git(path, &["check-ref-format", &format!("refs/heads/{name}")])
        .await
        .context("invalid source branch name")?;
    if remote.is_none() && !branch.starts_with("refs/remotes/") {
        if let Ok(commit) = git(
            path,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{name}^{{commit}}"),
            ],
        )
        .await
        {
            return Ok(commit.trim().into());
        }
        ensure!(
            !branch.starts_with("refs/heads/"),
            "local source branch does not exist: {branch}"
        );
    }
    let remotes = git(path, &["remote"]).await?;
    let mut remotes: Vec<&str> = remotes.lines().collect();
    // Longest match also handles remote names containing slashes.
    remotes.sort_by_key(|remote| std::cmp::Reverse(remote.len()));
    let qualified = branch.strip_prefix("refs/remotes/").unwrap_or(branch);
    let selected = if let Some(remote) = remote {
        ensure!(
            remotes.contains(&remote),
            "unknown configured remote: {remote}"
        );
        Some((remote, name))
    } else {
        remotes.iter().find_map(|remote| {
            qualified
                .strip_prefix(&format!("{remote}/"))
                .map(|name| (*remote, name))
        })
    };
    let (remote, name) = if let Some(selected) = selected {
        selected
    } else {
        ensure!(
            !branch.starts_with("refs/remotes/"),
            "unknown remote in source branch: {branch}"
        );
        let reference = format!("refs/heads/{name}");
        let mut matches = Vec::new();
        for remote in remotes {
            let refs = git(path, &["ls-remote", "--heads", "--", remote, &reference]).await
                .with_context(|| format!("could not inspect remote {remote}; use --remote to select the source explicitly"))?;
            if refs.lines().any(|line| {
                line.split_once('\t')
                    .is_some_and(|(_, found)| found == reference)
            }) {
                matches.push(remote);
            }
        }
        ensure!(
            !matches.is_empty(),
            "source branch not found locally or on configured remotes: {branch}"
        );
        ensure!(
            matches.len() == 1,
            "source branch exists on multiple remotes ({}); select one with --remote",
            matches.join(", ")
        );
        (matches[0], name)
    };
    let reference = format!("refs/heads/{name}");
    git(path, &["check-ref-format", &reference])
        .await
        .context("invalid remote branch name")?;
    git(
        path,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            remote,
            &format!("{reference}:{fetched}"),
        ],
    )
    .await?;
    Ok(git(
        path,
        &["rev-parse", "--verify", &format!("{fetched}^{{commit}}")],
    )
    .await?
    .trim()
    .into())
}

fn command(path: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "submodule.recurse=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

async fn git(path: &Path, args: &[&str]) -> Result<String> {
    let mut command = command(path);
    command.args(args);
    worktrunk::run(command).await
}
