//! Best-effort trust configuration shared by the launch adapters.
use anyhow::{Context as _, Result};
use serde_json::json;

use crate::{cli::context::Context, env};

/// Mark the workspace trusted in Claude Code's config so a launch, attached or
/// detached, never stops at the trust dialog. Failure only warns.
pub(super) fn trust_claude(ctx: &Context, workspace: &std::path::Path) {
    let trusted = env::claude_config_dir().and_then(|dir| {
        let config = dir
            .unwrap_or_else(|| ctx.paths.home.clone())
            .join(".claude.json");
        trust_claude_workspace(&config, workspace)
    });
    if let Err(error) = trusted {
        eprintln!("warning: could not mark the workspace as trusted for Claude Code: {error:#}");
    }
}

pub(super) fn trust_codex(ctx: &Context, workspace: &std::path::Path) {
    let trusted = env::codex_home().and_then(|dir| {
        let config = dir
            .unwrap_or_else(|| ctx.paths.home.join(".codex"))
            .join("config.toml");
        trust_codex_workspace(&config, workspace)
    });
    if let Err(error) = trusted {
        eprintln!("warning: could not mark the workspace as trusted for Codex: {error:#}");
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
    let text = match std::fs::read_to_string(config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".into(),
        Err(error) => return Err(error).with_context(|| format!("read {}", config.display())),
    };
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
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, &root)?;
    if let Ok(metadata) = std::fs::metadata(config) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(config)
        .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

fn trust_codex_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use std::io::Write;
    use toml_edit::{DocumentMut, Item, Table, value};

    let workspace = std::fs::canonicalize(workspace)?;
    let key = workspace.to_str().context("workspace path is not UTF-8")?;
    // Follow an existing config symlink instead of replacing it.
    let config = match std::fs::canonicalize(config) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => config.to_owned(),
        Err(error) => return Err(error).with_context(|| format!("resolve {}", config.display())),
    };
    let directory = config.parent().context("Codex config has no parent")?;
    let _lock = lock_trust_config(&config)?;
    let text = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", config.display())),
    };
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
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(root.to_string().as_bytes())?;
    if let Ok(metadata) = std::fs::metadata(&config) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(&config)
        .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

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
