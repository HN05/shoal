//! The Codex config a launch runs with, layer by layer as Codex reads it:
//! the user's `config.toml`, the selected profile's `<name>.config.toml`
//! beside it, then each trusted project's `.codex/config.toml`, from the
//! project's root down to the working directory. The launch's own `-c`
//! overrides go over them all.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The config Codex in `directory` reads from `codex_home` before the
/// launch's overrides, with `profile`, else the one the user's config
/// selects, layered over the user's.
pub(super) fn effective(
    codex_home: &Path,
    profile: Option<&str>,
    directory: &Path,
) -> Result<toml::Table> {
    let user = read(&codex_home.join("config.toml"))?;
    let profile = profile
        .map(str::to_owned)
        .or_else(|| user.get("profile")?.as_str().map(str::to_owned));
    let trusted = trusted(&user, directory);
    let mut config = user;
    if let Some(profile) = profile {
        merge(
            &mut config,
            read(&codex_home.join(format!("{profile}.config.toml")))?,
        );
    }
    if trusted {
        for project in project_directories(&config, directory) {
            merge(&mut config, read(&project.join(".codex/config.toml"))?);
        }
    }
    Ok(config)
}

/// Whether the user's config trusts the project `directory` is in, as its
/// own entry or one of a directory above it.
fn trusted(user: &toml::Table, directory: &Path) -> bool {
    let Some(projects) = user.get("projects").and_then(toml::Value::as_table) else {
        return false;
    };
    directory.ancestors().any(|ancestor| {
        let trust = |path: &Path| {
            projects
                .get(path.to_str()?)?
                .get("trust_level")?
                .as_str()
                .map(|level| level == "trusted")
        };
        trust(ancestor).unwrap_or(false)
    })
}

/// The directories whose project config applies in `directory`, from the
/// project root, the nearest one holding a root marker, down to
/// `directory` itself; only `directory` without a root.
fn project_directories(config: &toml::Table, directory: &Path) -> Vec<PathBuf> {
    let markers: Vec<String> = config
        .get("project_root_markers")
        .and_then(toml::Value::as_array)
        .map(|markers| {
            markers
                .iter()
                .filter_map(|marker| marker.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_else(|| vec![".git".to_owned()]);
    let root = directory
        .ancestors()
        .find(|ancestor| markers.iter().any(|marker| ancestor.join(marker).exists()));
    let Some(root) = root else {
        return vec![directory.to_owned()];
    };
    let mut directories: Vec<PathBuf> = directory
        .ancestors()
        .take_while(|ancestor| *ancestor != root)
        .map(Path::to_owned)
        .collect();
    directories.push(root.to_owned());
    directories.reverse();
    directories
}

/// Merge `layer` over `config`: tables key by key, anything else replaced.
fn merge(config: &mut toml::Table, layer: toml::Table) {
    for (key, value) in layer {
        match (config.get_mut(&key), value) {
            (Some(toml::Value::Table(below)), toml::Value::Table(above)) => merge(below, above),
            (_, value) => {
                config.insert(key, value);
            }
        }
    }
}

/// The Codex config at `path`, empty when there is none.
fn read(path: &Path) -> Result<toml::Table> {
    let Some(text) = crate::fsutil::read_optional(path)? else {
        return Ok(toml::Table::new());
    };
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests;
