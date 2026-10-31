//! Trust entries in the config files of the AI tools Shoal runs: the
//! directories each tool runs in without its trust dialog. Updates preserve
//! the tools' other settings and serialize between Shoal processes.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::json;

use crate::{
    env,
    fsutil::{self, Permissions, ReplaceOptions},
};

/// A tool's config file that records the directories it trusts.
pub struct TrustConfig {
    /// The tool's name in warnings.
    pub tool: &'static str,
    config: fn(&Path, &BTreeMap<String, OsString>) -> Result<PathBuf>,
    add: fn(&Path, &Path) -> Result<bool>,
    remove: fn(&Path, &[PathBuf]) -> Result<bool>,
}

pub static CLAUDE_CODE: TrustConfig = TrustConfig {
    tool: "Claude Code",
    config: claude_config,
    add: trust_claude_workspace,
    remove: remove_claude_trust,
};

pub static CODEX: TrustConfig = TrustConfig {
    tool: "Codex",
    config: codex_config,
    add: trust_codex_workspace,
    remove: remove_codex_trust,
};

impl std::fmt::Debug for TrustConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tool)
    }
}

impl TrustConfig {
    /// The config file the tool reads when it runs with the variables
    /// `configured` adds to this process's, under `home`.
    pub fn config(&self, home: &Path, configured: &BTreeMap<String, OsString>) -> Result<PathBuf> {
        (self.config)(home, configured)
    }

    /// Trust `directory` in `config`; returns whether the file changed.
    pub fn add(&self, config: &Path, directory: &Path) -> Result<bool> {
        (self.add)(config, directory)
    }

    /// Remove the entries for `paths` from `config`, keeping other project
    /// settings; returns whether the file changed.
    pub fn remove(&self, config: &Path, paths: &[PathBuf]) -> Result<bool> {
        (self.remove)(config, paths)
    }
}

fn claude_config(home: &Path, configured: &BTreeMap<String, OsString>) -> Result<PathBuf> {
    env::claude_config_dir_with(configured)
        .map(|dir| dir.unwrap_or_else(|| home.to_owned()).join(".claude.json"))
}

pub fn codex_config(home: &Path, configured: &BTreeMap<String, OsString>) -> Result<PathBuf> {
    env::codex_home_with(configured).map(|dir| {
        dir.unwrap_or_else(|| home.join(".codex"))
            .join("config.toml")
    })
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
