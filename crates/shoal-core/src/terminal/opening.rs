//! Opening places for Shoal's work: a restore of a workspace's agent, or
//! any Shoal command, with the variables a command in a new place needs,
//! running a command in an idle place an agent stopped in, and showing a
//! place to a user on another machine.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use super::{Beside, Configured, Elsewhere, Host, Places, Start, current_origin, named};
use crate::{env, paths::Paths, sessions::Origin};

/// Show `place`, which `host` opened focused, in this terminal when the
/// host does not serve it; a terminal it serves shows its focused place.
pub(crate) async fn show_opened(
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

/// Bring `place`, one a host opened, called `name`, before `viewer`, a user
/// on another machine whose terminal is the one Shoal made here for this
/// process, as its host shows a place there, or the session nested in it
/// where the viewer has no app for that host.
pub async fn show_place_elsewhere(
    place: &Origin,
    name: &str,
    viewer: &Elsewhere<'_>,
) -> Result<()> {
    let place = super::shown_elsewhere(place, viewer).await;
    let places = named(place.host).places().with_context(|| {
        format!(
            "{name} runs in {} on machine {}, which shows no places elsewhere; open it there",
            place.host, viewer.machine
        )
    })?;
    places.show_elsewhere(place, name, viewer).await
}

/// The variables a command in a new place needs from this process: its
/// output setting, which agents there inherit, and its home and config
/// location, whatever the environment the host starts places in.
pub fn place_env(paths: &Paths) -> Result<Vec<(&'static str, String)>> {
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
/// [`place_env`], the mark that tells the chat the place is its own, which
/// it records so its removal closes the place, and the queue entry whose
/// slot this process's agent takes, which the chat's agent takes instead.
pub fn chat_place_env(paths: &Paths) -> Result<Vec<(&'static str, String)>> {
    let mut env = place_env(paths)?;
    env.push((env::OPENED_CHAT_PLACE, "1".to_owned()));
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_string()));
    }
    Ok(env)
}

pub fn shoal_command(paths: &Paths, args: &[OsString]) -> Result<Vec<OsString>> {
    let mut argv = crate::workers::shoal_argv(paths)?;
    argv.extend_from_slice(args);
    Ok(argv)
}

/// Restore an agent in `stopped`, its earlier place, when that is in `host`
/// and idle.
pub(crate) async fn rerun_in_host(
    paths: &Paths,
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

/// Run `argv` at `cwd` with `env` in a new background place of `places`
/// `beside`, called `label`.
pub async fn open_in_background(
    paths: &Paths,
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
        nested_in: None,
    };
    places.open(&start).await.map(drop)
}
