//! Starting a program in a transient scope of its own, which the user's
//! systemd service manager places in a slice, apart from the control group
//! of the process starting it. The program keeps that process as its
//! parent, its terminal and its environment: `systemd-run --scope` moves
//! itself into the scope and then runs the program in its place.
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use tokio::process::Command;

use super::control::{COMMAND_TIMEOUT, query};
use crate::{fsutil::executables::find_executable, tools::Tool};

/// The first systemd whose `systemd-run` takes `--expand-environment`, which
/// otherwise expands `$` in a scope's arguments.
const UNEXPANDED_ARGUMENTS: u32 = 254;

/// The variables `systemd-run --user` finds the user's service manager by.
const BUS_VARIABLES: [&str; 2] = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];

/// The command that starts a program in a scope of its own in `slice`, or
/// in the service manager's default slice without one, which goes once its
/// processes have and outlives the OOM killer's kill of one of them; `None`
/// where `systemd-run` would change the program's arguments, or where it
/// cannot start a scope from this process, as without a user bus.
pub(crate) async fn launcher(slice: Option<&str>) -> Option<Vec<OsString>> {
    match query(Tool::SystemdRun, &["--version"]).await {
        Ok(version) if keeps_arguments(&version) => {}
        _ => return None,
    }
    let launcher = scope_command(connected_systemd_run()?, slice);
    starts_scopes(&launcher).await.then_some(launcher)
}

/// The command that runs `systemd-run` connected to the user's service
/// manager as this process is, whatever environment it is started in, such
/// as a caller's without the user bus: `env` setting the bus variables this
/// process has, which the started program inherits, then `systemd-run`, each
/// by its path on this process's `PATH`; `None` where either is missing.
fn connected_systemd_run() -> Option<Vec<OsString>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    systemd_run_with_bus(&path, |name| std::env::var_os(name))
}

/// [`connected_systemd_run`] for the search path `path` and the variables
/// `variable` reads.
fn systemd_run_with_bus(
    path: &OsStr,
    variable: impl Fn(&str) -> Option<OsString>,
) -> Option<Vec<OsString>> {
    let find =
        |program: &str| find_executable(OsStr::new(program), path).map(PathBuf::into_os_string);
    let mut command = vec![find("env")?];
    command.extend(BUS_VARIABLES.iter().filter_map(|name| {
        let mut assignment = OsString::from(format!("{name}="));
        assignment.push(variable(name)?);
        Some(assignment)
    }));
    command.push(find(Tool::SystemdRun.program())?);
    Some(command)
}

/// Whether `launcher` starts a program in an empty environment, which `true`
/// tries once.
async fn starts_scopes(launcher: &[OsString]) -> bool {
    let mut command = Command::new(&launcher[0]);
    command.args(&launcher[1..]).arg("true").env_clear();
    crate::subprocess::Run::new(command)
        .timeout(COMMAND_TIMEOUT)
        .capture()
        .await
        .is_ok_and(|output| output.status.success())
}

/// The command that starts a program in a scope of its own in `slice`
/// through `systemd_run`, the command that runs `systemd-run`.
fn scope_command(systemd_run: Vec<OsString>, slice: Option<&str>) -> Vec<OsString> {
    let mut launcher = systemd_run;
    launcher.extend(
        [
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "--property=OOMPolicy=continue",
        ]
        .map(OsString::from),
    );
    if let Some(slice) = slice {
        launcher.push(format!("--slice={slice}").into());
    }
    launcher.push("--".into());
    launcher
}

/// Whether the systemd that `systemd-run --version` printed `version` for
/// passes a scope's arguments on unchanged.
fn keeps_arguments(version: &str) -> bool {
    version
        .split_whitespace()
        .nth(1)
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number >= UNEXPANDED_ARGUMENTS)
}

#[cfg(test)]
mod tests;
