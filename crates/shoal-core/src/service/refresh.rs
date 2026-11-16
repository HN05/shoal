//! Keeping the installed service definition current from the daemon it
//! runs. The service manager reads only the definition `shoal daemon
//! install` wrote, so a release that changes it reaches an installed service
//! only through the daemon that runs under it.
use super::{
    Platform,
    control::command,
    definition::{file, launch, names_state_dir, render, write_definition},
};
use crate::{paths::Paths, tools::Tool};
use anyhow::Result;
use shoal_protocol::service::Definition;
use std::{fs, io, path::Path};

/// Replace the installed service definition of `paths`' state directory
/// with the one this release renders for the executable and `PATH` it
/// starts, which `shoal daemon install` chose, and have systemd reload it.
/// It restarts nothing: a running service applies what its manager applies
/// on reload, and the rest from its next start. Returns whether it replaced
/// the definition.
pub async fn refresh(paths: &Paths) -> Result<bool> {
    let platform = Platform::current()?;
    let Some(current) = replacement(paths, &file(paths, platform), platform)? else {
        return Ok(false);
    };
    if !rewrite(&current)? {
        return Ok(false);
    }
    if let Platform::Linux = platform {
        command(Tool::Systemctl, &["--user", "daemon-reload"], true).await?;
    }
    Ok(true)
}

/// The definition to write over the one installed at `path` when that one
/// runs this state directory's daemon and this release renders it
/// differently.
fn replacement(paths: &Paths, path: &Path, platform: Platform) -> Result<Option<Definition>> {
    let installed = match fs::read_to_string(path) {
        Ok(installed) => installed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !names_state_dir(paths, &installed, platform)? {
        return Ok(None);
    }
    let current = Definition {
        path: path.to_owned(),
        ..render(paths, &launch(&installed, platform)?, platform)?
    };
    Ok((current.content != installed).then_some(current))
}

/// Write `current` over the installed definition, unless a development
/// build, which leaves it to the installed Shoal.
fn rewrite(current: &Definition) -> Result<bool> {
    if !crate::build::changes_user_files() {
        return Ok(false);
    }
    write_definition(current)
}

#[cfg(test)]
mod tests;
