use super::*;
use serde_json::Value;
use serde_json::json;
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
