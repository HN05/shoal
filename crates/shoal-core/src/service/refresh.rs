//! Keeping the installed service definition current from the daemon it
//! runs. The service manager reads only the definition `shoal daemon
//! install` wrote, so a release that changes it reaches an installed service
//! only through the daemon that runs under it.
use super::{
    Platform,
    control::command,
    definition::{definition, executable, names_state_dir, write_definition},
};
use crate::{paths::Paths, tools::Tool};
use anyhow::Result;
use shoal_protocol::service::Definition;
use std::{fs, io};

/// Replace the installed service definition of `paths`' state directory
/// with the one `shoal daemon install` would write now, for the executable
/// this daemon was started through and the `PATH` it was given, and have
/// systemd reload it. It restarts nothing: a running service applies what
/// its manager applies on reload, and the rest from its next start. Returns
/// whether it replaced the definition.
pub async fn refresh(paths: &Paths) -> Result<bool> {
    let platform = Platform::current()?;
    let current = definition(paths, &executable(None)?, platform)?;
    if !rewrite(paths, &current, platform)? {
        return Ok(false);
    }
    if let Platform::Linux = platform {
        command(Tool::Systemctl, &["--user", "daemon-reload"], true).await?;
    }
    Ok(true)
}

/// Write `current` over the installed definition when it runs this state
/// directory's daemon and differs; a development build leaves it to the
/// installed Shoal.
fn rewrite(paths: &Paths, current: &Definition, platform: Platform) -> Result<bool> {
    if !crate::build::changes_user_files() || !outdated(paths, current, platform)? {
        return Ok(false);
    }
    write_definition(current)
}

/// Whether a definition is installed at `current`'s path for this state
/// directory with other content.
fn outdated(paths: &Paths, current: &Definition, platform: Platform) -> Result<bool> {
    let installed = match fs::read_to_string(&current.path) {
        Ok(installed) => installed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(installed != current.content && names_state_dir(paths, &installed, platform)?)
}

#[cfg(test)]
mod tests;
