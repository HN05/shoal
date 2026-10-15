//! Best-effort trust configuration shared by the launch adapters.
use crate::fsutil::{self, Permissions, ReplaceOptions};
use anyhow::{Context as _, Result};
use serde_json::json;

use crate::{
    cli::{client, context::Context},
    env,
    model::{Repository, Workspace},
};

/// Remove trust entries for a repository after its deletion succeeds.
/// Config cleanup is best effort and never makes repository removal fail.
pub(crate) fn cleanup_repository_trust(
    ctx: &Context,
    repository: &Repository,
    workspace_paths: &[std::path::PathBuf],
) {
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
    let claude = env::claude_config_dir().map(|dir| {
        dir.unwrap_or_else(|| ctx.paths.home.clone())
            .join(".claude.json")
    });
    cleanup_config(claude, &paths, "Claude Code", remove_claude_trust);
    let codex = env::codex_home().map(|dir| {
        dir.unwrap_or_else(|| ctx.paths.home.join(".codex"))
            .join("config.toml")
    });
    cleanup_config(codex, &paths, "Codex", remove_codex_trust);
}

fn cleanup_config(
    config: Result<std::path::PathBuf>,
    paths: &[std::path::PathBuf],
    agent: &str,
    remove: fn(&std::path::Path, &[std::path::PathBuf]) -> Result<bool>,
) {
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: could not clean {agent} trust config: {error:#}");
            return;
        }
    };
    if let Err(error) = remove(&config, paths) {
        eprintln!(
            "warning: could not clean {agent} trust config {}: {error:#}",
            config.display()
        );
    }
}

/// Mark the workspace trusted in Claude Code's config so a launch, attached or
/// detached, never stops at the trust dialog. Failure only warns.
pub(super) async fn trust_claude(ctx: &Context, workspace: &Workspace) {
    let config = env::claude_config_dir().map(|dir| {
        dir.unwrap_or_else(|| ctx.paths.home.clone())
            .join(".claude.json")
    });
    trust_launch(
        ctx,
        workspace,
        config,
        "Claude Code",
        trust_claude_workspace,
    )
    .await;
}

pub(super) async fn trust_codex(ctx: &Context, workspace: &Workspace) {
    let config = env::codex_home().map(|dir| {
        dir.unwrap_or_else(|| ctx.paths.home.join(".codex"))
            .join("config.toml")
    });
    trust_launch(ctx, workspace, config, "Codex", trust_codex_workspace).await;
}

async fn trust_launch(
    ctx: &Context,
    workspace: &Workspace,
    config: Result<std::path::PathBuf>,
    agent: &str,
    update: fn(&std::path::Path, &std::path::Path) -> Result<bool>,
) {
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: could not mark the workspace as trusted for {agent}: {error:#}");
            return;
        }
    };
    let mut paths = Vec::new();
    match client::repositories(&ctx.paths).await.and_then(|repos| {
        repos
            .into_iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("workspace repository is missing")
    }) {
        Ok(repo) => {
            paths.extend(repository_trust_paths(&repo, &workspace.path));
        }
        Err(error) => {
            eprintln!("warning: could not resolve repository trust paths for {agent}: {error:#}");
            paths.push(workspace.path.clone());
        }
    }
    for path in paths {
        if let Err(error) = update(&config, &path) {
            eprintln!(
                "warning: could not mark {} as trusted for {agent}: {error:#}",
                path.display()
            );
        }
    }
}

/// Trust the shared workspaces directory for normal Shoal worktrees. An
/// explicitly placed worktree outside it needs its own project entry.
fn repository_trust_paths(
    repository: &Repository,
    workspace: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    match &repository.workspaces_dir {
        Some(directory) if workspace.starts_with(directory) => vec![directory.clone()],
        Some(directory) => vec![directory.clone(), workspace.to_owned()],
        None => vec![workspace.to_owned()],
    }
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
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn trust_updates_preserve_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        for claude in [false, true] {
            fs::write(&config, if claude { "{}" } else { "" }).unwrap();
            fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
            if claude {
                trust_claude_workspace(&config, &workspace).unwrap();
            } else {
                trust_codex_workspace(&config, &workspace).unwrap();
            }
            assert_eq!(
                fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn cleanup_removes_trust_fields_but_preserves_project_settings() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let other = dir.path().join("other");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&other).unwrap();
        let workspace = fs::canonicalize(workspace).unwrap();
        let other = fs::canonicalize(other).unwrap();
        let paths = vec![workspace.clone()];

        let claude = dir.path().join("claude.json");
        fs::write(
            &claude,
            serde_json::to_vec(&json!({
                "projects": {
                    workspace.to_str().unwrap(): {
                        "hasTrustDialogAccepted": true,
                        "allowedTools": ["Bash"]
                    },
                    other.to_str().unwrap(): {"hasTrustDialogAccepted": true}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(remove_claude_trust(&claude, &paths).unwrap());
        let root: Value = serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
        assert_eq!(
            root["projects"][workspace.to_str().unwrap()]["allowedTools"],
            json!(["Bash"])
        );
        assert!(root["projects"][workspace.to_str().unwrap()]["hasTrustDialogAccepted"].is_null());
        assert_eq!(
            root["projects"][other.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );

        let codex = dir.path().join("codex.toml");
        fs::write(
            &codex,
            format!(
                "[projects]\n{} = {{ trust_level = 'trusted', custom = 42 }}\n{} = {{ trust_level = 'trusted' }}\n",
                toml_edit::Key::new(workspace.to_str().unwrap()),
                toml_edit::Key::new(other.to_str().unwrap())
            ),
        )
        .unwrap();
        assert!(remove_codex_trust(&codex, &paths).unwrap());
        let root: toml::Value = toml::from_str(&fs::read_to_string(&codex).unwrap()).unwrap();
        assert_eq!(
            root["projects"][workspace.to_str().unwrap()]["custom"].as_integer(),
            Some(42)
        );
        assert!(
            root["projects"][workspace.to_str().unwrap()]
                .get("trust_level")
                .is_none()
        );
        assert_eq!(
            root["projects"][other.to_str().unwrap()]["trust_level"].as_str(),
            Some("trusted")
        );
    }

    #[test]
    fn concurrent_trust_updates_keep_every_workspace() {
        for claude in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("codex/config.toml");
            let workspaces: Vec<_> = (0..8)
                .map(|index| {
                    let path = dir.path().join(format!("workspace-{index}"));
                    fs::create_dir(&path).unwrap();
                    fs::canonicalize(path).unwrap()
                })
                .collect();
            let barrier = std::sync::Barrier::new(workspaces.len());
            std::thread::scope(|scope| {
                for path in &workspaces {
                    scope.spawn(|| {
                        barrier.wait();
                        if claude {
                            trust_claude_workspace(&config, path).unwrap();
                        } else {
                            trust_codex_workspace(&config, path).unwrap();
                        }
                    });
                }
            });
            let text = fs::read_to_string(config).unwrap();
            let root: Value = if claude {
                serde_json::from_str(&text).unwrap()
            } else {
                serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap()
            };
            for path in workspaces {
                let project = &root["projects"][path.to_str().unwrap()];
                if claude {
                    assert_eq!(project["hasTrustDialogAccepted"], true);
                } else {
                    assert_eq!(project["trust_level"], "trusted");
                }
            }
        }
    }

    #[test]
    fn codex_trust_preserves_settings_comments_and_config_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let link = dir.path().join("linked.toml");
        let workspace = dir.path().join("ws.with 'quotes' and \"quotes\"");
        fs::create_dir(&workspace).unwrap();
        let key = fs::canonicalize(&workspace).unwrap();
        let key = key.to_str().unwrap();
        // An existing inline entry exercises table-like access and quoted keys.
        let original = format!(
            "# Keep my settings\nmodel = 'custom'\n[projects]\n'/other' = {{ trust_level = 'untrusted' }}\n{} = {{ trust_level = 'untrusted', custom = 42 }}\n",
            toml_edit::Key::new(key)
        );
        fs::write(&config, &original).unwrap();
        std::os::unix::fs::symlink(&config, &link).unwrap();
        assert!(trust_codex_workspace(&link, &workspace).unwrap());
        assert!(link.is_symlink());
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.starts_with("# Keep my settings\nmodel = 'custom'\n"));
        let root: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            root["projects"][key]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(root["projects"][key]["custom"].as_integer(), Some(42));
        assert_eq!(
            root["projects"]["/other"]["trust_level"].as_str(),
            Some("untrusted")
        );
        assert!(!trust_codex_workspace(&config, &workspace).unwrap());
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
        for bad in [
            "invalid TOML".to_owned(),
            "projects = []".to_owned(),
            format!("[projects]\n{} = 1", toml_edit::Key::new(key)),
        ] {
            fs::write(&config, &bad).unwrap();
            assert!(trust_codex_workspace(&config, &workspace).is_err());
            assert_eq!(fs::read_to_string(&config).unwrap(), bad);
        }
    }

    #[test]
    fn claude_trust_adds_the_project_once_and_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config/.claude.json");
        let workspace = dir.path().join("ws");
        fs::create_dir(&workspace).unwrap();
        assert!(trust_claude_workspace(&config, &workspace).unwrap());
        let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        let key = fs::canonicalize(&workspace).unwrap();
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        fs::write(
            &config,
            r#"{"numStartups": 3, "projects": {"/other": {"allowedTools": ["Bash"], "hasTrustDialogAccepted": false}}}"#,
        )
        .unwrap();
        assert!(trust_claude_workspace(&config, &workspace).unwrap());
        assert!(!trust_claude_workspace(&config, &workspace).unwrap());
        let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(root["numStartups"], 3);
        assert_eq!(root["projects"]["/other"]["allowedTools"], json!(["Bash"]));
        assert_eq!(root["projects"]["/other"]["hasTrustDialogAccepted"], false);
        let key = fs::canonicalize(&workspace).unwrap();
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        fs::write(&config, "{not json").unwrap();
        assert!(trust_claude_workspace(&config, &workspace).is_err());
        fs::write(&config, r#"{"projects": []}"#).unwrap();
        assert!(trust_claude_workspace(&config, &workspace).is_err());
    }
}
