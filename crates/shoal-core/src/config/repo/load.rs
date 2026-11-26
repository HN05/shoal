//! Reading a repository's config layers: the worktree's file and the local
//! file, with the scripts and templates they name.
use crate::config::hooks::HookCommand;
use crate::config::modes;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use super::RepoConfig;

/// The repository-owned configuration layers before they are resolved. Keeping
/// them separate lets callers explain where an effective value came from.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ConfigLayers {
    pub worktree_file: RepoConfig,
    /// The file in the repository's own directory, outside every Git tree.
    pub local_config: RepoConfig,
    /// The Markdown file each mode's instructions or rule's text of a layer
    /// came from.
    #[serde(default)]
    pub worktree_template_files: modes::Files,
    #[serde(default)]
    pub local_template_files: modes::Files,
    /// Why a workspace's worktree file is left out: the current format
    /// rejects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_worktree_file: Option<String>,
    /// The secrets the repository's webhook triggers sign with that these
    /// layers may not name, such as those of its checkout's file, which a
    /// workspace's own file can lack; no agent receives them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub signing_secrets: BTreeSet<String>,
}

pub fn parse(text: &str) -> Result<RepoConfig> {
    let config = crate::config::parse::repository(text)?;
    config.validate()?;
    Ok(config)
}

/// The directory in a repository's own directory that holds its local
/// config and the scripts it names, beside the workspaces and `.checkout`.
pub const LOCAL_DIR: &str = ".shoal";

pub fn local_config_path(repository_dir: &Path) -> PathBuf {
    repository_dir.join(LOCAL_DIR).join("config.toml")
}

/// The claims the repository's reviews raised that were resolved as wrong.
pub fn false_positives_path(repository_dir: &Path) -> PathBuf {
    repository_dir.join(LOCAL_DIR).join("false-positives.toml")
}

/// The local config of the repository whose directory is `repository_dir`,
/// read in place, with the mode and rule files beside it and the files
/// they came from.
/// Relative hook paths and `setup.command` name scripts in its directory, so
/// they become absolute.
pub fn load_local(repository_dir: &Path) -> Result<(RepoConfig, modes::Files)> {
    let path = local_config_path(repository_dir);
    let directory = repository_dir.join(LOCAL_DIR);
    let text =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config = match text {
        Some(text) => parse(&text).with_context(|| format!("parse {}", path.display()))?,
        None => RepoConfig::default(),
    };
    resolve_script_paths(&mut config, &directory);
    let files = modes::fill(&mut config, &[directory])?;
    Ok((config, files))
}

pub(super) fn resolve_script_paths(config: &mut RepoConfig, directory: &Path) {
    for &kind in crate::config::hooks::HookKind::ALL {
        if let Some(command) = kind.repository_command_mut(config) {
            *command = directory.join(&*command).to_string_lossy().into_owned();
        }
    }
}

/// The error of a worktree's config that the current format rejects, as
/// opposed to one that could not be read. Its message names the file.
#[derive(Debug)]
pub struct Rejected(String);

impl Rejected {
    /// Whether `error` is a rejection, with whatever context it gained since.
    pub fn is(error: &anyhow::Error) -> bool {
        error.downcast_ref::<Self>().is_some()
    }
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// The worktree's repository config with its mode and rule files, and the
/// files they came from. A file the format rejects fails with [`Rejected`].
pub fn load(workspace_dir: &Path) -> Result<(RepoConfig, modes::Files)> {
    let paths = [
        workspace_dir.join(".shoal.toml"),
        workspace_dir.join(".shoal/config.toml"),
    ];
    let found: Vec<_> = paths.iter().filter(|p| p.exists()).collect();
    if found.len() > 1 {
        return Err(Rejected(
            "both .shoal.toml and .shoal/config.toml exist; keep only one repository config".into(),
        )
        .into());
    }
    let mut config = match found.first() {
        Some(path) => {
            let text =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            parse(&text).with_context(|| Rejected(format!("parse {}", path.display())))?
        }
        None => RepoConfig::default(),
    };
    let files = modes::fill(
        &mut config,
        &[workspace_dir.to_owned(), workspace_dir.join(".shoal")],
    )?;
    Ok((config, files))
}
