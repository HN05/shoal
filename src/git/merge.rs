//! Git merges run inside a tracked execution, keeping terminal I/O and child
//! processes in the CLI wrapper. The daemon authorizes the destination workspace.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        client,
        context::Context,
        internal::{InternalCommand, internal_command},
        ui::{self, Fallback},
    },
    env, execution,
    git::{
        self,
        fetch::{self, FetchPolicy},
        run_without_submodules,
    },
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
    let command = internal_command(
        &ctx.paths,
        ctx.json,
        InternalCommand::Merge {
            branch: &branch,
            remote: remote.as_deref(),
            local,
        },
    )?;
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
    let previous = run_without_submodules(path, &["rev-parse", "HEAD"]).await?;
    fetch::with_temporary_ref(path, "merge", run_without_submodules, async |fetched| {
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
        let commit = source(path, &branch, remote.as_deref(), fetched).await?;
        own_branch(path, &workspace.branch).await?;
        ensure!(
            run_without_submodules(path, &["rev-parse", "HEAD"]).await? == previous,
            "workspace HEAD changed during fetch; retry shoal merge"
        );
        let output = git::merge_commit(path, &branch, &commit)
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
                    "commit": run_without_submodules(path, &["rev-parse", "HEAD"]).await?.trim(),
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
    })
    .await
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
    if run_without_submodules(path, &["rev-parse", "--verify", &git::local_ref(name)])
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
        git::head_branch(path, true, run_without_submodules)
            .await?
            .as_deref()
            == Some(branch),
        "workspace must be on its own recorded branch ({branch}) before merging"
    );
    Ok(())
}

/// Resolve the source branch to a commit, fetching it into `fetched` when it
/// only exists on a remote.
async fn source(path: &Path, branch: &str, remote: Option<&str>, fetched: &str) -> Result<String> {
    let name = git::strip_local(branch).unwrap_or(branch);
    run_without_submodules(path, &["check-ref-format", &git::local_ref(name)])
        .await
        .context("invalid source branch name")?;
    if remote.is_none() && git::strip_remote(branch).is_none() {
        if let Ok(commit) =
            git::resolve_commit(path, &git::local_ref(name), run_without_submodules).await
        {
            return Ok(commit);
        }
        ensure!(
            git::strip_local(branch).is_none(),
            "local source branch does not exist: {branch}"
        );
    }
    let remotes = run_without_submodules(path, &["remote"]).await?;
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
    run_without_submodules(path, &["check-ref-format", &reference])
        .await
        .context("invalid remote branch name")?;
    fetch::fetch_commit(
        path,
        remote,
        &reference,
        fetched,
        FetchPolicy::PrivateOnly,
        run_without_submodules,
    )
    .await
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
        let refs = run_without_submodules(
            path,
            &["ls-remote", "--heads", "--", remote, &reference],
        )
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
