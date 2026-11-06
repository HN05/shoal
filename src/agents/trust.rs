//! Best-effort trust entries in the config files of the tools Shoal launches.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::json;

use super::{ADDED, AgentEnvironment, integration};
use crate::{
    ai::IntegrationKind,
    client::transcripts::Environments,
    env,
    fsutil::{self, Permissions, ReplaceOptions},
    model::Repository,
};

/// A tool's config file that records the directories it trusts, so a
/// launch never stops at the tool's trust dialog. Updates are best effort and
/// only warn.
pub(crate) struct TrustFile {
    /// The tool's name in warnings.
    tool: &'static str,
    /// The config file the tool reads when it runs with the environment.
    config: fn(&AgentEnvironment) -> Result<std::path::PathBuf>,
    add: fn(&std::path::Path, &std::path::Path) -> Result<bool>,
    remove: fn(&std::path::Path, &[std::path::PathBuf]) -> Result<bool>,
}

pub(crate) static CLAUDE_CODE: TrustFile = TrustFile {
    tool: "Claude Code",
    config: claude_config,
    add: trust_claude_workspace,
    remove: remove_claude_trust,
};

pub(crate) static CODEX: TrustFile = TrustFile {
    tool: "Codex",
    config: codex_config,
    add: trust_codex_workspace,
    remove: remove_codex_trust,
};

impl TrustFile {
    /// The tool's name in warnings.
    pub(crate) fn tool(&self) -> &'static str {
        self.tool
    }

    /// Trust `paths` in the config file the tool reads when it runs with
    /// `environment`; each failure comes back as a warning.
    pub(crate) fn trust(
        &self,
        environment: &AgentEnvironment,
        paths: &[std::path::PathBuf],
    ) -> Vec<String> {
        let config = match (self.config)(environment) {
            Ok(config) => config,
            Err(error) => {
                return vec![format!(
                    "could not mark directories as trusted for {}: {error:#}",
                    self.tool
                )];
            }
        };
        paths
            .iter()
            .filter_map(|path| {
                (self.add)(&config, path).err().map(|error| {
                    format!(
                        "could not mark {} as trusted for {}: {error:#}",
                        path.display(),
                        self.tool
                    )
                })
            })
            .collect()
    }

    /// Remove the entries for `paths` from the config file of each of
    /// `environments`, while preserving other project settings.
    fn forget(
        &self,
        environments: &[AgentEnvironment],
        paths: &[std::path::PathBuf],
    ) -> Vec<String> {
        let (configs, mut warnings) = self.configs(environments);
        for config in configs {
            if let Err(error) = (self.remove)(&config, paths) {
                warnings.push(format!(
                    "could not clean {} trust config {}: {error:#}",
                    self.tool,
                    config.display()
                ));
            }
        }
        warnings
    }

    /// The distinct config files the tool reads with `environments`, and a
    /// warning for each environment that names none.
    fn configs(&self, environments: &[AgentEnvironment]) -> (Vec<std::path::PathBuf>, Vec<String>) {
        let mut configs = Vec::new();
        let mut warnings = Vec::new();
        for environment in environments {
            match (self.config)(environment) {
                Ok(config) if !configs.contains(&config) => configs.push(config),
                Ok(_) => {}
                Err(error) => warnings.push(format!(
                    "could not clean {} trust config: {error:#}",
                    self.tool
                )),
            }
        }
        (configs, warnings)
    }
}

/// Remove a deleted repository's trust entries from the configs of this
/// process's environment and of `agent_environments`, under `home`; each
/// failure comes back as a warning.
pub(crate) fn forget_repository_trust(
    home: &Path,
    repository: &Repository,
    workspace_paths: &[PathBuf],
    agent_environments: &[BTreeMap<String, String>],
) -> Vec<String> {
    let paths = repository_paths(repository, workspace_paths);
    let environments = Environments::new(home, agent_environments);
    let environments = environments.each();
    let mut cleaned: Vec<&TrustFile> = Vec::new();
    let mut warnings = Vec::new();
    let integrations = IntegrationKind::ALL
        .iter()
        .map(|kind| integration(*kind))
        .chain(ADDED.iter().map(|added| added.integration));
    for integration in integrations {
        let Some(trust) = integration.trust_file() else {
            continue;
        };
        if !cleaned.iter().any(|done| std::ptr::eq(*done, trust)) {
            warnings.extend(trust.forget(&environments, &paths));
            cleaned.push(trust);
        }
    }
    warnings
}

/// The directories a repository's removal stops trusting: its checkout, its
/// workspaces directory and its workspaces.
fn repository_paths(
    repository: &Repository,
    workspace_paths: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for path in std::iter::once(&repository.path)
        .chain(repository.workspaces_dir.iter())
        .chain(workspace_paths.iter())
    {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn claude_config(environment: &AgentEnvironment) -> Result<std::path::PathBuf> {
    env::claude_config_dir_from(|name| environment.variable(name)).map(|dir| {
        dir.unwrap_or_else(|| environment.home.to_owned())
            .join(".claude.json")
    })
}

pub(super) fn codex_config(environment: &AgentEnvironment) -> Result<std::path::PathBuf> {
    env::codex_home_from(|name| environment.variable(name)).map(|dir| {
        dir.unwrap_or_else(|| environment.home.join(".codex"))
            .join("config.toml")
    })
}

/// Trust the repository's checkout and the shared workspaces directory for
/// normal Shoal worktrees. Claude Code keys every worktree's trust by the
/// checkout and searches parents only up to the worktree root; the directory
/// covers versions that search every parent. An explicitly placed worktree
/// outside it needs its own project entry.
pub(crate) fn repository_trust_paths(
    repository: &Repository,
    workspace: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut paths = vec![repository.path.clone()];
    paths.extend(repository.workspaces_dir.iter().cloned());
    let shared = repository
        .workspaces_dir
        .as_ref()
        .is_some_and(|directory| workspace.starts_with(directory));
    if !shared && !paths.iter().any(|path| path == workspace) {
        paths.push(workspace.to_owned());
    }
    paths
}

/// Record the workspace as trusted in Claude Code's `.claude.json` so
/// `claude` starts without its workspace trust dialog. Returns whether the
/// file changed, creating it when needed.
fn trust_claude_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use serde_json::Value;
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_owned());
    let key = workspace
        .to_str()
        .context("workspace path is not UTF-8")?
        .to_owned();
    let _lock = lock_trust_config(config)?;
    let text = crate::fsutil::read_optional(config)
        .with_context(|| format!("read {}", config.display()))?
        .unwrap_or_else(|| "{}".into());
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", config.display()))?;
    let project = root
        .as_object_mut()
        .context("Claude config is not a JSON object")?
        .entry("projects")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude config `projects` is not a JSON object")?
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude project entry is not a JSON object")?;
    if project.get("hasTrustDialogAccepted") == Some(&Value::Bool(true)) {
        return Ok(false);
    }
    project.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    let directory = config.parent().context("Claude config has no parent")?;
    std::fs::create_dir_all(directory)?;
    fsutil::replace_atomically(
        config,
        &serde_json::to_vec_pretty(&root)?,
        ReplaceOptions {
            permissions: Permissions::Preserve,
            sync: false,
        },
    )
    .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

fn remove_claude_trust(config: &std::path::Path, paths: &[std::path::PathBuf]) -> Result<bool> {
    use serde_json::Value;

    let Some((config, _lock)) = lock_existing_trust_config(config)? else {
        return Ok(false);
    };
    let text = crate::fsutil::read_optional(&config)
        .with_context(|| format!("read {}", config.display()))?
        .unwrap_or_else(|| "{}".into());
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", config.display()))?;
    let Some(projects) = root.get_mut("projects") else {
        return Ok(false);
    };
    let projects = projects
        .as_object_mut()
        .context("Claude config `projects` is not a JSON object")?;
    let mut changed = false;
    for path in paths {
        let key = path.to_str().context("trust path is not UTF-8")?;
        let remove_project = if let Some(project) = projects.get_mut(key) {
            let project = project
                .as_object_mut()
                .context("Claude project entry is not a JSON object")?;
            changed |= project.remove("hasTrustDialogAccepted").is_some();
            project.is_empty()
        } else {
            false
        };
        if remove_project {
            projects.remove(key);
        }
    }
    if changed {
        fsutil::replace_atomically(
            &config,
            &serde_json::to_vec_pretty(&root)?,
            ReplaceOptions {
                permissions: Permissions::Preserve,
                sync: false,
            },
        )
        .with_context(|| format!("replace {}", config.display()))?;
    }
    Ok(changed)
}

fn trust_codex_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use toml_edit::{DocumentMut, Item, Table, value};

    let workspace = std::fs::canonicalize(workspace)?;
    let key = workspace.to_str().context("workspace path is not UTF-8")?;
    // Follow an existing config symlink instead of replacing it.
    let config = match std::fs::canonicalize(config) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => config.to_owned(),
        Err(error) => return Err(error).with_context(|| format!("resolve {}", config.display())),
    };
    config.parent().context("Codex config has no parent")?;
    let _lock = lock_trust_config(&config)?;
    let text = crate::fsutil::read_optional(&config)
        .with_context(|| format!("read {}", config.display()))?
        .unwrap_or_default();
    let mut root: DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", config.display()))?;
    let project = root
        .entry("projects")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("Codex config `projects` is not a TOML table")?
        .entry(key)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("Codex project entry is not a TOML table")?;
    if project.get("trust_level").and_then(Item::as_str) == Some("trusted") {
        return Ok(false);
    }
    project.insert("trust_level", value("trusted"));
    fsutil::replace_atomically(
        &config,
        root.to_string().as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Preserve,
            sync: false,
        },
    )
    .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

fn remove_codex_trust(config: &std::path::Path, paths: &[std::path::PathBuf]) -> Result<bool> {
    use toml_edit::DocumentMut;

    let Some((config, _lock)) = lock_existing_trust_config(config)? else {
        return Ok(false);
    };
    let text = crate::fsutil::read_optional(&config)
        .with_context(|| format!("read {}", config.display()))?
        .unwrap_or_default();
    let mut root: DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", config.display()))?;
    let Some(projects) = root.get_mut("projects") else {
        return Ok(false);
    };
    let projects = projects
        .as_table_like_mut()
        .context("Codex config `projects` is not a TOML table")?;
    let mut changed = false;
    for path in paths {
        let key = path.to_str().context("trust path is not UTF-8")?;
        let remove_project = if let Some(project) = projects.get_mut(key) {
            let project = project
                .as_table_like_mut()
                .context("Codex project entry is not a TOML table")?;
            changed |= project.remove("trust_level").is_some();
            project.is_empty()
        } else {
            false
        };
        if remove_project {
            projects.remove(key);
        }
    }
    if changed {
        fsutil::replace_atomically(
            &config,
            root.to_string().as_bytes(),
            ReplaceOptions {
                permissions: Permissions::Preserve,
                sync: false,
            },
        )
        .with_context(|| format!("replace {}", config.display()))?;
    }
    Ok(changed)
}

// This lock coordinates Shoal processes; the agents do not use it themselves.
fn lock_trust_config(config: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::create_dir_all(config.parent().context("agent config has no parent")?)?;
    let mut path = config.as_os_str().to_os_string();
    path.push(".shoal-lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

fn lock_existing_trust_config(
    config: &std::path::Path,
) -> Result<Option<(std::path::PathBuf, std::fs::File)>> {
    let config = match std::fs::canonicalize(config) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("resolve {}", config.display())),
    };
    Ok(Some((config.clone(), lock_trust_config(&config)?)))
}

#[cfg(test)]
mod tests;
