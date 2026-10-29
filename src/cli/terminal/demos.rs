//! Demos in the places of the first host that keeps them, otherwise as
//! background processes.
use anyhow::Result;

use super::{
    background, groups,
    here::current_origin,
    host::{Demo, DemoWorker, Host},
    hosts::{named, names_places},
    launch::place_env,
};
use crate::{
    cli::context::Context, config::Effective, demos::Launch, env, model::Workspace,
    sessions::Origin,
};

/// Variables a demo's worker must not inherit from the agent that starts it:
/// the agent's execution marker would make the agent's exit stop the demo.
pub(super) const NOT_INHERITED_BY_DEMOS: &[&str] = &[env::EXECUTION_ID];

/// Start a demo of `workspace` in the first host that runs queued starts and
/// keeps demos, otherwise as a background process with a log. `worker` gives
/// the demo worker's argv for the place it runs in, which this returns.
pub(in crate::cli) async fn start_demo(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    name: &str,
    launch: &str,
    worker: &DemoWorker<'_>,
) -> Result<Origin> {
    let origin = current_origin();
    let repo = groups::Repo::Id(&workspace.repository_id);
    let env = place_env(ctx)?;
    let started = groups::first_queued(
        ctx,
        settings,
        origin.as_ref(),
        &repo,
        &workspace.path,
        async |host, group| {
            let Some(demos) = host.host.demos() else {
                return Ok(None);
            };
            if host.host.ensure_installed().is_err() {
                return Ok(None);
            }
            let demo = Demo {
                launch,
                state: &ctx.paths.state,
                origin: group,
                workspace: &workspace.id,
                cwd: &workspace.path,
                label: &demo_label(host.host, workspace, name),
                env: &env,
                worker,
            };
            demos.start_demo(&demo).await.map(Some)
        },
    )
    .await?;
    match started {
        Some(place) => Ok(place),
        None => background::start_demo(&ctx.paths, workspace, name, worker),
    }
}

/// What the place `host` opens for demo `name` of `workspace` is called:
/// readable where the host shows names, otherwise an identifier.
fn demo_label(host: &dyn Host, workspace: &Workspace, name: &str) -> String {
    if names_places(host) {
        format!("{} demo {name}", workspace.name)
    } else {
        format!("{}-{name}", workspace.name)
    }
}

/// Show the demo `launch` describes in this terminal.
pub(in crate::cli) async fn open_demo(launch: &Launch) -> Result<()> {
    let place = &launch.origin;
    match named(place.host).places() {
        Some(places) => places.show(place, "the demo").await,
        None => background::open_demo(place.id()).await,
    }
}

/// What the demo `launch` describes shows.
pub(in crate::cli) async fn demo_screen(launch: &Launch) -> Result<String> {
    let place = &launch.origin;
    match named(place.host).demos() {
        Some(demos) => demos.demo_screen(place).await,
        None => background::demo_screen(place.id()),
    }
}

/// Type `keys` into the demo `launch` describes.
pub(in crate::cli) async fn send_demo_keys(launch: &Launch, keys: &str) -> Result<()> {
    match named(launch.origin.host).types() {
        Some(types) => types.send_keys(&launch.origin, keys).await,
        None => background::send_demo_keys(),
    }
}
