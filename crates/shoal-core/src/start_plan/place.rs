//! Opening a place for a plan: the place worker, which carries out the
//! plan, a recorded space's agent or a workspace's shell, in a new place of
//! a terminal host.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::PlacePlan;
use crate::{
    env,
    model::Space,
    paths::Paths,
    terminal::{Beside, Configured, Host, Places, Start, names_places, place_env, show_opened},
};

/// Have `host` run the place worker for `plan`, a start in `space`, in a
/// new place of its `places` `beside`, `focus`ed or in the background.
/// The worker reads the plan from the place's environment, and a place
/// that keeps a shell gets the directive through which the worker has it
/// enter a workspace.
pub async fn open_place(
    paths: &Paths,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    plan: &PlacePlan,
    space: &Space,
    focus: bool,
) -> Result<()> {
    let worker = place_worker(paths, host, focus)?;
    let directive = places
        .places_keep_shells()
        .then(ShellDirective::create)
        .transpose()?;
    // A new workspace's worktree may not exist yet.
    let home = Some(space.workdir.path()).filter(|path| path.is_dir());
    let cwd = home.map_or_else(std::env::current_dir, |home| Ok(home.to_owned()))?;
    let start = Start {
        state: &paths.state,
        origin: beside.origin,
        cwd: &cwd,
        label: &place_label(host.host, plan, space),
        focus,
        split: beside.split,
        argv: &worker,
        env: &plan_env(paths, plan, directive.as_ref())?,
        nested_in: None,
    };
    let place = places.open(&start).await?;
    if let Some(directive) = directive {
        directive.hand_over()?;
    }
    if start.focus {
        show_opened(host, places, &place, start.label).await?;
    }
    Ok(())
}

/// The worker that carries out a plan in the place `host` opened for it,
/// `focused` or not, closing the place with a workspace when config says
/// so. The plan reaches it in the place's environment.
fn place_worker(paths: &Paths, host: &Configured, focused: bool) -> Result<Vec<OsString>> {
    crate::workers::internal_command(
        paths,
        false,
        crate::workers::Worker::Place {
            host: Some(host.host.name()),
            close_when_done: host.closes_when_done,
            focused,
        },
    )
}

/// What the place `host` opens for `plan` in `space` is called: the plan's
/// label where the host shows names, otherwise the space's name.
fn place_label(host: &dyn Host, plan: &PlacePlan, space: &Space) -> String {
    if names_places(host) {
        plan.label(space)
    } else {
        space.name.to_string()
    }
}

/// The variables of the place that carries out `plan`: a new place's, the
/// plan, and the `directive` of a place that keeps a shell.
fn plan_env(
    paths: &Paths,
    plan: &PlacePlan,
    directive: Option<&ShellDirective>,
) -> Result<Vec<(&'static str, String)>> {
    let mut env = place_env(paths)?;
    env.push((env::PLACE_PLAN, serde_json::to_string(plan)?));
    if let Some(directive) = directive {
        env.push((env::SHELL_DIRECTIVE, directive.path()));
    }
    Ok(env)
}

/// The file in which the place worker leaves the workspace for the shell of
/// its place to enter. Shoal removes it until the place runs its worker;
/// then the shell removes it at its next prompt, or the watcher when it
/// closes the place.
struct ShellDirective(tempfile::TempPath);

impl ShellDirective {
    fn create() -> Result<Self> {
        let file = tempfile::Builder::new()
            .prefix("shoal-cd.")
            .tempfile()
            .context("create the place's shell directive")?;
        Ok(Self(file.into_temp_path()))
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }

    fn hand_over(self) -> Result<()> {
        self.0.keep().context("keep the place's shell directive")?;
        Ok(())
    }
}
