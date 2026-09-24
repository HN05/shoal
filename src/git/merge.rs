//! Git merges run inside a tracked execution, keeping terminal I/O and child
//! processes in the CLI wrapper. The daemon authorizes the destination workspace.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    env, execution, git,
    model::PulledBranch,
    protocol::Method,
};

/// Re-invoke this binary as `merge-internal` through the execution wrapper.
pub async fn run(
    ctx: &Context,
    workspace: Option<String>,
    branch: String,
    remote: Option<String>,
    local: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let mut command = vec![std::env::current_exe()?.into_os_string()];
    if ctx.json {
        command.push("--json".into());
    }
    command.extend(["merge-internal".into(), branch.into()]);
    if let Some(remote) = remote {
        command.extend(["--remote".into(), remote.into()]);
    }
    if local {
        command.push("--local".into());
    }
    execution::run(&ctx.paths, workspace, command, None).await
}

pub async fn worker(
    ctx: &Context,
    branch: String,
    remote: Option<String>,
    local: bool,
) -> Result<i32> {
    ensure!(env::is_scoped(), "merge worker requires a scoped execution");
    let workspace =
        std::env::var(env::WORKSPACE_ID).context("merge worker requires a workspace")?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    ensure!(
        std::env::current_dir()?.canonicalize()? == workspace.path.canonicalize()?,
        "merge worker must run in its own worktree"
    );
    let path = &workspace.path;
    own_branch(path, &workspace.branch).await?;
    let previous = git_run(path, &["rev-parse", "HEAD"]).await?;
    let fetched = format!("refs/shoal/merge/{}", Uuid::new_v4());
    let result = async {
        let refresh = if local || remote.is_some() {
            None
        } else {
            refresh_source(ctx, path, &workspace.id, &branch).await?
        };
        if let Some(refresh) = &refresh
            && !ctx.json
        {
            eprintln!("{}", refresh_summary(refresh));
        }
        let commit = source(path, &branch, remote.as_deref(), &fetched).await?;
        own_branch(path, &workspace.branch).await?;
        ensure!(
            git_run(path, &["rev-parse", "HEAD"]).await? == previous,
            "workspace HEAD changed during fetch; retry shoal merge"
        );
        let output = git_command(path)
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
        let exit_code = execution::exit_code(output.status);
        if ctx.json {
            println!(
                "{}",
                json!({
                    "workspace_id": workspace.id,
                    "source_refresh": refresh,
                    "source_commit": commit,
                    "previous_commit": previous.trim(),
                    "commit": git_run(path, &["rev-parse", "HEAD"]).await?.trim(),
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
    let cleanup = git_run(path, &["update-ref", "-d", &fetched]).await;
    let code = result?;
    cleanup.context("could not remove temporary merge ref")?;
    Ok(code)
}

/// Ask the daemon to fast-forward an existing local source branch from its
/// upstream, so the merge imports current work. Remote-qualified sources
/// fetch fresh data on their own, and missing local branches are discovered
/// on remotes by `source`.
async fn refresh_source(
    ctx: &Context,
    path: &Path,
    workspace: &str,
    branch: &str,
) -> Result<Option<PulledBranch>> {
    if git::strip_remote(branch).is_some() {
        return Ok(None);
    }
    let name = git::strip_local(branch).unwrap_or(branch);
    if git_run(path, &["rev-parse", "--verify", &git::local_ref(name)])
        .await
        .is_err()
    {
        return Ok(None);
    }
    let refresh = client::request::<PulledBranch>(
        &ctx.paths,
        Method::RefreshMergeSource {
            workspace: workspace.to_owned(),
            branch: name.to_owned(),
        },
    )
    .await
    .with_context(|| {
        format!(
            "could not refresh {name} from its upstream; pass --local to merge the local branch as it is"
        )
    })?;
    Ok(Some(refresh))
}

fn refresh_summary(refresh: &PulledBranch) -> String {
    match (&refresh.skipped, refresh.updated) {
        (Some(skipped), _) => skipped.clone(),
        (None, true) => format!(
            "Updated {} from its upstream ({}..{})",
            refresh.branch, refresh.previous_commit, refresh.commit
        ),
        (None, false) => format!("{} is up to date with its upstream", refresh.branch),
    }
}

async fn own_branch(path: &Path, branch: &str) -> Result<()> {
    ensure!(
        git::head_branch(path, true, git_run).await?.as_deref() == Some(branch),
        "workspace must be on its own recorded branch ({branch}) before merging"
    );
    Ok(())
}

/// Resolve the source branch to a commit, fetching it into `fetched` when it
/// only exists on a remote.
async fn source(path: &Path, branch: &str, remote: Option<&str>, fetched: &str) -> Result<String> {
    let name = git::strip_local(branch).unwrap_or(branch);
    git_run(path, &["check-ref-format", &git::local_ref(name)])
        .await
        .context("invalid source branch name")?;
    if remote.is_none() && git::strip_remote(branch).is_none() {
        if let Ok(commit) = git::resolve_commit(path, &git::local_ref(name), git_run).await {
            return Ok(commit);
        }
        ensure!(
            git::strip_local(branch).is_none(),
            "local source branch does not exist: {branch}"
        );
    }
    let remotes = git_run(path, &["remote"]).await?;
    let mut remotes: Vec<&str> = remotes.lines().collect();
    // Longest match also handles remote names containing slashes.
    remotes.sort_by_key(|remote| std::cmp::Reverse(remote.len()));
    let qualified = git::strip_remote(branch).unwrap_or(branch);
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
    let (remote, name) = match selected {
        Some(selected) => selected,
        None => {
            ensure!(
                git::strip_remote(branch).is_none(),
                "unknown remote in source branch: {branch}"
            );
            (find_remote_with_branch(path, &remotes, name).await?, name)
        }
    };
    let reference = git::local_ref(name);
    git_run(path, &["check-ref-format", &reference])
        .await
        .context("invalid remote branch name")?;
    git_run(
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
    git::resolve_commit(path, fetched, git_run).await
}

/// The single configured remote advertising `refs/heads/<name>`.
async fn find_remote_with_branch<'a>(
    path: &Path,
    remotes: &[&'a str],
    name: &str,
) -> Result<&'a str> {
    let reference = git::local_ref(name);
    let mut matches = Vec::new();
    for remote in remotes {
        let refs = git_run(path, &["ls-remote", "--heads", "--", remote, &reference])
            .await
            .with_context(|| {
                format!(
                    "could not inspect remote {remote}; use --remote to select the source explicitly"
                )
            })?;
        if refs.lines().any(|line| {
            line.split_once('\t')
                .is_some_and(|(_, found)| found == reference)
        }) {
            matches.push(*remote);
        }
    }
    ensure!(
        !matches.is_empty(),
        "source branch not found locally or on configured remotes: {name}"
    );
    ensure!(
        matches.len() == 1,
        "source branch exists on multiple remotes ({}); select one with --remote",
        matches.join(", ")
    );
    Ok(matches[0])
}

fn git_command(path: &Path) -> Command {
    let mut command = git::isolated_command(path);
    command.args(["-c", "submodule.recurse=false"]);
    command
}

async fn git_run(path: &Path, args: &[&str]) -> Result<String> {
    let mut command = git_command(path);
    command.args(args);
    crate::subprocess::output(command).await
}
