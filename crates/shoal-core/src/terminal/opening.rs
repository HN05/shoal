//! Opening places for Shoal's work: the worker that adds a planned
//! workspace, a restore of a workspace's agent, or any Shoal command, with
//! the variables a command in a new place needs, and running a command in
//! an idle place an agent stopped in.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use super::{Beside, Configured, Host, Places, Start, current_origin, names_places, naming};
use crate::{env, model::Workspace, paths::CommandPaths, sessions::Origin, start_plan::AddPlan};

/// Have `host` run the add worker for `plan`, the planned workspace `name`,
/// in a new place of its `places` `beside`, `focus`ed or in the background.
/// The worker reads the plan from the place's environment, and a place
/// that keeps a shell gets the directive through which the worker has it
/// enter the workspace.
pub async fn open_add(
    paths: &CommandPaths,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    plan: &AddPlan,
    name: &str,
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
        label: &add_label(host.host, plan, name),
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
fn add_worker(paths: &CommandPaths, host: &Configured, focused: bool) -> Result<Vec<OsString>> {
    crate::workers::internal_command(
        paths,
        false,
        crate::workers::Worker::Add {
            host: host.host.name(),
            close_when_done: host.closes_when_done,
            focused,
        },
    )
}

/// Show `place`, which `host` opened focused, in this terminal when the
/// host does not serve it; a terminal it serves shows its focused place.
pub async fn show_opened(
    host: &Configured,
    places: &dyn Places,
    place: &Origin,
    name: &str,
) -> Result<()> {
    if host.serves() {
        return Ok(());
    }
    places.show(place, name).await
}

/// What the place `host` opens for `plan` is called: the plan's label where
/// the host shows names, otherwise the planned workspace `name`.
fn add_label(host: &dyn Host, plan: &AddPlan, name: &str) -> String {
    if names_places(host) {
        plan.label()
    } else {
        name.to_owned()
    }
}

/// The variables of the place that adds `plan`: a new place's, the plan,
/// and the `directive` of a place that keeps a shell.
fn add_env(
    paths: &CommandPaths,
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

/// The variables a command in a new place needs from this process: its
/// output setting, which agents there inherit, and its home and config
/// location, whatever the environment the host starts places in.
pub fn place_env(paths: &CommandPaths) -> Result<Vec<(&'static str, String)>> {
    let config = crate::config::Config::path(paths);
    let config_home = config
        .parent()
        .and_then(Path::parent)
        .context("config has no parent")?;
    Ok(vec![
        (env::OUTPUT, env::output_setting().1),
        ("HOME", paths.home.display().to_string()),
        ("XDG_CONFIG_HOME", config_home.display().to_string()),
    ])
}

/// The variables a chat in a place Shoal opens for it needs: those of
/// [`place_env`], and the mark that tells the chat the place is its own, which
/// it records so its removal closes the place.
pub fn chat_place_env(paths: &CommandPaths) -> Result<Vec<(&'static str, String)>> {
    let mut env = place_env(paths)?;
    env.push((env::OPENED_CHAT_PLACE, "1".to_owned()));
    Ok(env)
}

/// This executable with `args`, as a place runs it.
pub fn shoal_command(paths: &CommandPaths, args: &[OsString]) -> Result<Vec<OsString>> {
    let mut argv = crate::workers::shoal_argv(paths)?;
    argv.extend_from_slice(args);
    Ok(argv)
}

/// Restore an agent in `stopped`, its earlier place, when that is in `host`
/// and idle.
pub async fn rerun_in_host(
    paths: &CommandPaths,
    host: &dyn Host,
    stopped: Option<&Origin>,
    args: &[OsString],
) -> Result<bool> {
    let Some(places) = host.places() else {
        return Ok(false);
    };
    let Some(place) = reusable_place(stopped).filter(|place| place.host == host.name()) else {
        return Ok(false);
    };
    let argv = shoal_command(paths, args)?;
    places
        .run_in_place(&paths.state, place, &argv, &place_env(paths)?)
        .await
}

/// `stopped`, where an agent stopped, unless that is where this process
/// runs, which is never reused.
pub(super) fn reusable_place(stopped: Option<&Origin>) -> Option<&Origin> {
    let here = current_origin().and_then(|origin| origin.place);
    stopped.filter(|origin| origin.place.is_some() && origin.place != here)
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
    let label = naming::workspace_label(paths, template, host.host, workspace).await;
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

/// Run `argv` at `cwd` with `env` in a new background place of `places`
/// `beside`, called `label`.
pub(super) async fn open_in_background(
    paths: &CommandPaths,
    places: &dyn Places,
    beside: Beside<'_>,
    cwd: &Path,
    label: &str,
    argv: &[OsString],
    env: &[(&str, String)],
) -> Result<()> {
    let start = Start {
        state: &paths.state,
        origin: beside.origin,
        cwd,
        label,
        focus: false,
        split: beside.split,
        argv,
        env,
    };
    places.open(&start).await.map(drop)
}
