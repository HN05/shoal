//! Demos in the places of the first host that keeps them; a demo needs a
//! terminal host.
use anyhow::{Context as _, Result};

use super::{groups, launch::place_env};
use crate::{
    cli::context::Context,
    config::Effective,
    demos::Launch,
    model::Workspace,
    sessions::Origin,
    terminal::{Demo, DemoWorker, Host, current_origin, named, names_places},
};

/// Start a demo of `workspace` in the first host that runs queued starts and
/// keeps demos, failing with the last host's reason when none does. `worker`
/// gives the demo worker's argv for the place it runs in, which this returns.
pub(in crate::cli) async fn start_demo(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    name: &str,
    launch: &str,
    worker: &DemoWorker<'_>,
) -> Result<Origin> {
    let origin = current_origin();
    let repo = groups::Repo::Id(workspace.repository_id.as_str());
    let env = place_env(ctx)?;
    groups::first_queued(
        ctx,
        settings,
        origin.as_ref(),
        &repo,
        &workspace.path,
        async |host, group| {
            let Some(demos) = host.host.demos() else {
                return Ok(None);
            };
            host.host.ensure_installed()?;
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
    .await?
    .context(NO_DEMO_HOST)
}

const NO_DEMO_HOST: &str = "no terminal host can start demos";

/// What the place `host` opens for demo `name` of `workspace` is called:
/// readable where the host shows names, otherwise an identifier.
fn demo_label(host: &dyn Host, workspace: &Workspace, name: &str) -> String {
    if names_places(host) {
        format!("{} demo {name}", workspace.name)
    } else {
        format!("{}-{name}", workspace.directory_name())
    }
}

/// Show the demo `launch` describes in this terminal.
pub(in crate::cli) async fn open_demo(launch: &Launch) -> Result<()> {
    let place = &launch.origin;
    named(place.host)
        .places()
        .with_context(|| format!("{} cannot show demos", place.host))?
        .show(place, "the demo")
        .await
}

/// What the demo `launch` describes shows.
pub(in crate::cli) async fn demo_screen(launch: &Launch) -> Result<String> {
    let place = &launch.origin;
    named(place.host)
        .demos()
        .with_context(|| format!("{} cannot read demos", place.host))?
        .demo_screen(place)
        .await
}

/// Type `keys` into the demo `launch` describes.
pub(in crate::cli) async fn send_demo_keys(launch: &Launch, keys: &str) -> Result<()> {
    let place = &launch.origin;
    named(place.host)
        .types()
        .with_context(|| format!("{} cannot type into demos", place.host))?
        .send_keys(place, keys)
        .await
}
