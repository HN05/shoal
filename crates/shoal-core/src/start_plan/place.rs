//! Opening a place for a plan: the add worker, which carries out the plan
//! in a new place of a terminal host.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::AddPlan;
use crate::{
    env,
    model::Workspace,
    paths::Paths,
    terminal::{Beside, Configured, Host, Places, Start, names_places, place_env, show_opened},
};

/// Have `host` run the add worker for `plan`, a start in `workspace`, in a
/// new place of its `places` `beside`, `focus`ed or in the background.
/// The worker reads the plan from the place's environment, and a place
/// that keeps a shell gets the directive through which the worker has it
/// enter the workspace.
pub async fn open_add(
    paths: &Paths,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    plan: &AddPlan,
    workspace: &Workspace,
    focus: bool,
) -> Result<()> {
    let worker = add_worker(paths, host, focus)?;
    let directive = places
        .places_keep_shells()
        .then(ShellDirective::create)
        .transpose()?;
    let start = Start {
        state: &paths.state,
        origin: beside.origin,
        cwd: &std::env::current_dir()?,
        label: &add_label(host.host, plan, workspace),
        focus,
        split: beside.split,
        argv: &worker,
        env: &add_env(paths, plan, directive.as_ref())?,
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

/// The worker that adds the workspace a plan describes in the place `host`
/// opened for it, `focused` or not, closing the place with the workspace
/// when config says so. The plan reaches it in the place's environment.
fn add_worker(paths: &Paths, host: &Configured, focused: bool) -> Result<Vec<OsString>> {
    crate::workers::internal_command(
        paths,
        false,
        crate::workers::Worker::Add {
            host: Some(host.host.name()),
            close_when_done: host.closes_when_done,
            focused,
        },
    )
}

/// What the place `host` opens for `plan` in `workspace` is called: the
/// plan's label where the host shows names, otherwise the workspace's name.
fn add_label(host: &dyn Host, plan: &AddPlan, workspace: &Workspace) -> String {
    if names_places(host) {
        plan.label(workspace)
    } else {
        workspace.name.to_string()
    }
}

/// The variables of the place that adds `plan`: a new place's, the plan,
/// and the `directive` of a place that keeps a shell.
fn add_env(
    paths: &Paths,
    plan: &AddPlan,
    directive: Option<&ShellDirective>,
) -> Result<Vec<(&'static str, String)>> {
    let mut env = place_env(paths)?;
    env.push((env::ADD_PLAN, serde_json::to_string(plan)?));
    if let Some(directive) = directive {
        env.push((env::SHELL_DIRECTIVE, directive.path()));
    }
    Ok(env)
}

/// The file in which the add worker leaves the workspace for the shell of
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
