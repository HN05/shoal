//! Best-effort trust entries in the config files of the tools Shoal launches.
use crate::fsutil::{self, Permissions, ReplaceOptions};
use anyhow::{Context as _, Result};
use serde_json::json;

use super::AgentEnvironment;
use crate::{
    cli::{client, commands::workspaces, context::Context},
    env,
    model::{Repository, Workspace},
};

/// A tool's config file that records the directories it trusts, so a
/// launch never stops at the tool's trust dialog. Updates are best effort and
/// only warn.
pub(in crate::cli) struct TrustFile {
    /// The tool's name in warnings.
    tool: &'static str,
    /// The config file the tool reads when it runs with the environment.
    config: fn(&AgentEnvironment) -> Result<std::path::PathBuf>,
    add: fn(&std::path::Path, &std::path::Path) -> Result<bool>,
    remove: fn(&std::path::Path, &[std::path::PathBuf]) -> Result<bool>,
}

/// Whose agents ran with a config location: a workspace's, or a chat's in
/// the repository's checkout.
enum Holder<'a> {
    Workspace(&'a Workspace),
    Repository(&'a Repository),
}

pub(super) static CLAUDE_CODE: TrustFile = TrustFile {
    tool: "Claude Code",
    config: claude_config,
    add: trust_claude_workspace,
    remove: remove_claude_trust,
};

pub(super) static CODEX: TrustFile = TrustFile {
    tool: "Codex",
    config: codex_config,
    add: trust_codex_workspace,
    remove: remove_codex_trust,
};

impl TrustFile {
    /// Trust the workspace's repository directories for the tool running
    /// with `environment`.
    pub(super) async fn trust_workspace(
        &self,
        ctx: &Context,
        workspace: &Workspace,
        environment: &AgentEnvironment<'_>,
    ) {
        let paths = workspace_trust_paths(ctx, workspace, self.tool).await;
        self.trust(environment, &paths);
    }

    /// [`Self::trust_workspace`] for a tracked agent, which runs with the
    /// workspace's `[env]` changed by `profile`, after the daemon keeps
    /// where that puts the tool's config for repository removal.
    pub(super) async fn trust_tracked(
        &self,
        ctx: &Context,
        workspace: &Workspace,
        profile: &crate::ai::Environment,
    ) {
        let mut configured = workspaces::agent_environment(ctx, workspace).await;
        profile.layer(&mut configured, &ctx.paths.home);
        self.keep_config_location(ctx, Holder::Workspace(workspace), &configured)
            .await;
        let environment = AgentEnvironment {
            home: &ctx.paths.home,
            configured: &configured,
        };
        self.trust_workspace(ctx, workspace, &environment).await;
    }

    /// Have the daemon keep where `configured` puts the tool's config for
    /// the removal of `holder`'s repository. Failure only warns.
    async fn keep_config_location(
        &self,
        ctx: &Context,
        holder: Holder<'_>,
        configured: &std::collections::BTreeMap<String, std::ffi::OsString>,
    ) {
        let environment = env::agent_config_environment(configured);
        if environment.is_empty() {
            return;
        }
        let kept = match holder {
            Holder::Workspace(workspace) => {
                client::keep_agent_environment(&ctx.paths, &workspace.id, environment).await
            }
            Holder::Repository(repository) => {
                client::keep_repository_agent_environment(&ctx.paths, &repository.id, environment)
                    .await
            }
        };
        if let Err(error) = kept {
            eprintln!(
                "warning: repository removal may leave {} trust entries behind: {error:#}",
                self.tool
            );
        }
    }

    /// Trust a repository's checkout for a chat, which runs with this
    /// process's environment changed by `profile`, as workspace launches
    /// trust theirs; repository removal clears the entries.
    pub(super) async fn trust_checkout(
        &self,
        ctx: &Context,
        repository: &Repository,
        profile: &crate::ai::Environment,
    ) {
        let mut configured = std::collections::BTreeMap::new();
        profile.layer(&mut configured, &ctx.paths.home);
        self.keep_config_location(ctx, Holder::Repository(repository), &configured)
            .await;
        let environment = AgentEnvironment {
            home: &ctx.paths.home,
            configured: &configured,
        };
        self.trust(
            &environment,
            &repository_trust_paths(repository, &repository.path),
        );
    }

    fn trust(&self, environment: &AgentEnvironment, paths: &[std::path::PathBuf]) {
        let config = match (self.config)(environment) {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "warning: could not mark directories as trusted for {}: {error:#}",
                    self.tool
                );
                return;
            }
        };
        for path in paths {
            if let Err(error) = (self.add)(&config, path) {
                eprintln!(
                    "warning: could not mark {} as trusted for {}: {error:#}",
                    path.display(),
                    self.tool
                );
            }
        }
    }

    /// Remove the entries for `paths` from the config file of each of
    /// `environments`, while preserving other project settings.
    pub(super) fn forget(
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

/// The directories a repository's removal stops trusting: its checkout, its
/// workspaces directory and its workspaces.
pub(super) fn repository_paths(
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
    env::claude_config_dir_with(environment.configured).map(|dir| {
        dir.unwrap_or_else(|| environment.home.to_owned())
            .join(".claude.json")
    })
}

fn codex_config(environment: &AgentEnvironment) -> Result<std::path::PathBuf> {
    env::codex_home_with(environment.configured).map(|dir| {
        dir.unwrap_or_else(|| environment.home.join(".codex"))
            .join("config.toml")
    })
}

/// The workspace's repository trust paths, or the workspace alone when its
/// repository cannot be resolved.
async fn workspace_trust_paths(
    ctx: &Context,
    workspace: &Workspace,
    agent: &str,
) -> Vec<std::path::PathBuf> {
    match client::repositories(&ctx.paths).await.and_then(|repos| {
        repos
            .into_iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("workspace repository is missing")
    }) {
        Ok(repo) => repository_trust_paths(&repo, &workspace.path),
        Err(error) => {
            eprintln!("warning: could not resolve repository trust paths for {agent}: {error:#}");
            vec![workspace.path.clone()]
        }
    }
}

/// Trust the repository's checkout and the shared workspaces directory for
/// normal Shoal worktrees. Claude Code keys every worktree's trust by the
/// checkout and searches parents only up to the worktree root; the directory
/// covers versions that search every parent. An explicitly placed worktree
/// outside it needs its own project entry.
fn repository_trust_paths(
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
