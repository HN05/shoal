//! The starts the daemon runs later for a terminal that queued them, which
//! has none of its own: each opens its place in the background where that
//! terminal would have, falling back as [`groups::first_queued`] does.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use super::groups;
use crate::{
    agents::HomeRecords,
    client::connection::CommandPaths,
    config::Effective,
    model::{PresentSpace, Repository, Workspace},
    start_plan::AddPlan,
    terminal::{
        Beside, Configured, Places, chat_place_env, open_add, open_in_background, place_env,
        queued::{NO_QUEUED_HOST, queued_origin},
        shoal_command,
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// Start an admitted queue entry's `plan`, the planned workspace `name`, in
/// the background where the terminal that queued it would have.
pub async fn start_add(
    paths: &CommandPaths,
    settings: &Effective,
    plan: &mut AddPlan,
    name: &str,
) -> Result<()> {
    let origin = queued_origin()?;
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::Selected(plan.repository.clone());
    groups::first_queued(
        paths,
        settings,
        origin.as_ref(),
        &repo,
        &cwd,
        async |host, group| {
            let Some(places) = host.places() else {
                return Ok(None);
            };
            let template = host.options.name_template.clone();
            super::naming::name_plan(paths, template, plan, None).await?;
            let beside = Beside::opening(group);
            open_add(paths, host, places, beside, plan, name, false).await?;
            Ok(Some(()))
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
}

/// Run Shoal with `args`, which restore an agent of `workspace`, for the
/// daemon's worker, which has no terminal: in the background of the first
/// place a queued start from there can open.
pub async fn restore_in_background(
    paths: &CommandPaths,
    settings: &Effective,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<()> {
    let origin = queued_origin()?;
    let repo = groups::Repo::Id(workspace.repository_id.as_str());
    groups::first_queued(
        paths,
        settings,
        origin.as_ref(),
        &repo,
        &workspace.path,
        async |host, group| {
            let Some(places) = host.places() else {
                return Ok(None);
            };
            start_in_background(paths, host, places, Beside::opening(group), workspace, args)
                .await
                .map(Some)
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
}

/// Run Shoal's `argv` for the chat `name` of `repository`, or of none, in
/// `cwd`, its checkout, worktree or `root_dir`, in a new background place,
/// of the first host that would start a queued agent, for the user to
/// attach to. A `restore` records the place as the chat's, as a
/// workspace's restore does. Returns how that host reports it.
pub async fn start_chat(
    paths: &CommandPaths,
    settings: &Effective,
    repository: Option<&Repository>,
    cwd: &Path,
    name: &str,
    argv: Vec<OsString>,
    restore: bool,
) -> Result<String> {
    let origin = queued_origin()?;
    let repo = groups::Repo::of_chat(repository, cwd);
    groups::first_queued(
        paths,
        settings,
        origin.as_ref(),
        &repo,
        cwd,
        async |host, group| {
            let Some(places) = host.places() else {
                return Ok(None);
            };
            let beside = Beside::opening(group);
            let mut argv = argv.clone();
            if restore {
                let host_name = host.host.name().as_str();
                argv.extend(["--opened-in".into(), host_name.into()]);
                if host.closes_when_done {
                    argv.extend(["--close-in".into(), host_name.into()]);
                }
            }
            let env = chat_place_env(paths)?;
            open_in_background(paths, places, beside, cwd, name, &argv, &env).await?;
            Ok(Some(format!(
                "Started {name} {}",
                places.background_place(name)
            )))
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
}

/// Run Shoal with `args`, a restore of the agent of the chat `space`, in a
/// new background place of the first host that would start a queued agent,
/// in its home.
pub async fn restore_chat(
    paths: &CommandPaths,
    settings: &Effective,
    space: &PresentSpace,
    args: &[OsString],
) -> Result<()> {
    let records = HomeRecords::read(paths, &SpaceSelector::id(space.id())).await?;
    let home = records.home()?;
    let repository = match &records {
        HomeRecords::Chat { repository, .. } => repository.as_ref(),
        HomeRecords::Workspace(_) => None,
    };
    let name = space.name().as_str();
    let argv = shoal_command(paths, args)?;
    start_chat(paths, settings, repository, home.path(), name, argv, true).await?;
    Ok(())
}

/// Run Shoal with `args`, a restore of `workspace`, in a new background
/// place of `host`'s `places` `beside`, telling the restore to close
/// the place with the workspace when config says so.
pub async fn start_in_background(
    paths: &CommandPaths,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<()> {
    let template = host.options.name_template.clone();
    let label = super::naming::workspace_label(paths, template, host.host, workspace).await;
    let name = host.host.name().as_str();
    // The restore records its place before anything can run there.
    let mut argv = shoal_command(paths, args)?;
    argv.extend(["--opened-in".into(), name.into()]);
    if host.closes_when_done {
        argv.extend(["--close-in".into(), name.into()]);
    }
    let env = place_env(paths)?;
    open_in_background(paths, places, beside, &workspace.path, &label, &argv, &env).await
}
