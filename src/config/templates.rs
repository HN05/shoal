//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};

use crate::{agent::BuiltinAgent, config::Config, paths::Paths};

pub use super::placeholders::render;

pub const ISSUE_FILE: &str = "issue-template.md";
pub const ISSUE_DEFAULT: &str = include_str!("../../issue-template.md");
pub const AGENT_FILE: &str = "agent-template.md";
pub const AGENT_DEFAULT: &str = include_str!("../../agent-template.md");
/// Earlier shipped agent defaults. An installed copy that still matches one was
/// never edited, so `install` replaces it; add the old text here when changing it.
const AGENT_RETIRED: &[&str] = &[
    include_str!("templates/retired/agent-1.md"),
    include_str!("templates/retired/agent-2.md"),
    include_str!("templates/retired/agent-3.md"),
    include_str!("templates/retired/agent-4.md"),
    include_str!("templates/retired/agent-5.md"),
    include_str!("templates/retired/agent-6.md"),
    include_str!("templates/retired/agent-7.md"),
    include_str!("templates/retired/agent-8.md"),
    include_str!("templates/retired/agent-9.md"),
    include_str!("templates/retired/agent-10.md"),
];

pub fn read(directory: &Path, name: &str) -> Result<Option<String>> {
    let path = directory.join(name);
    crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))
}

pub fn install(paths: &Paths) -> Result<()> {
    let config = Config::path(paths);
    install_at(config.parent().context("config has no directory")?)
}

fn install_at(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    for (name, contents, retired) in [
        (ISSUE_FILE, ISSUE_DEFAULT, &[][..]),
        (AGENT_FILE, AGENT_DEFAULT, AGENT_RETIRED),
    ] {
        let path = directory.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(contents.as_bytes())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                refresh_unedited(&path, contents, retired)?
            }
            Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
        }
    }
    Ok(())
}

/// Replace a regular file that still holds a retired default; symlinks and
/// edited files belong to the user.
fn refresh_unedited(path: &Path, contents: &str, retired: &[&str]) -> Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Ok(());
    }
    let installed = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if retired.contains(&installed.as_str()) {
        crate::fsutil::replace_atomically(
            path,
            contents.as_bytes(),
            crate::fsutil::ReplaceOptions {
                permissions: crate::fsutil::Permissions::Preserve,
                sync: false,
            },
        )
        .with_context(|| format!("update {}", path.display()))?;
    }
    Ok(())
}

pub fn instructions(template: Option<&str>, workspace: &crate::model::Workspace) -> String {
    render(
        template.unwrap_or_default(),
        &[
            ("{workspace}", &workspace.name),
            ("{branch}", &workspace.branch),
            ("{path}", &workspace.path.to_string_lossy()),
        ],
    )
}

pub fn instruction_args(agent: BuiltinAgent, instructions: String) -> Vec<std::ffi::OsString> {
    if instructions.is_empty() {
        return Vec::new();
    }
    match agent {
        BuiltinAgent::Claude => {
            vec!["--append-system-prompt".into(), instructions.into()]
        }
        BuiltinAgent::Codex => vec![
            "-c".into(),
            format!(
                "developer_instructions={}",
                toml::Value::String(instructions)
            )
            .into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_seeds_missing_templates_and_preserves_edits() {
        let directory = tempfile::tempdir().unwrap();
        install_at(directory.path()).unwrap();
        let path = directory.path().join(ISSUE_FILE);
        assert_eq!(fs::read_to_string(&path).unwrap(), ISSUE_DEFAULT);
        fs::write(&path, "custom").unwrap();
        let agent = directory.path().join(AGENT_FILE);
        assert_eq!(fs::read_to_string(&agent).unwrap(), AGENT_DEFAULT);
        fs::remove_file(&agent).unwrap();
        install_at(directory.path()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "custom");
        assert_eq!(fs::read_to_string(&agent).unwrap(), AGENT_DEFAULT);
        fs::write(&agent, "custom agent").unwrap();
        install_at(directory.path()).unwrap();
        assert_eq!(fs::read_to_string(agent).unwrap(), "custom agent");
    }

    #[test]
    fn install_refreshes_only_unedited_retired_agent_templates() {
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join(AGENT_FILE);
        for retired in AGENT_RETIRED {
            assert_ne!(*retired, AGENT_DEFAULT);
            fs::write(&agent, retired).unwrap();
            install_at(directory.path()).unwrap();
            assert_eq!(fs::read_to_string(&agent).unwrap(), AGENT_DEFAULT);
        }
        let edited = format!("{}Project note.\n", AGENT_RETIRED[0]);
        fs::write(&agent, &edited).unwrap();
        install_at(directory.path()).unwrap();
        assert_eq!(fs::read_to_string(&agent).unwrap(), edited);
        // A linked template belongs to the user even when it holds a default.
        let linked = directory.path().join("dotfiles-agent.md");
        fs::write(&linked, AGENT_RETIRED[0]).unwrap();
        fs::remove_file(&agent).unwrap();
        std::os::unix::fs::symlink(&linked, &agent).unwrap();
        install_at(directory.path()).unwrap();
        assert!(fs::symlink_metadata(&agent).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&linked).unwrap(), AGENT_RETIRED[0]);
    }

    #[test]
    fn native_instruction_arguments_preserve_empty_and_literal_text() {
        for agent in BuiltinAgent::ALL {
            assert!(instruction_args(*agent, String::new()).is_empty());
        }
        let text = "quotes: \" and newlines\n$(false)";
        assert_eq!(
            instruction_args(BuiltinAgent::Claude, text.into()),
            vec![
                std::ffi::OsString::from("--append-system-prompt"),
                text.into()
            ]
        );
    }

    #[test]
    fn codex_instructions_are_a_literal_toml_string() {
        let text = "quotes: \"'''\\\nUnicode: 日本語 $(false)";
        let args = instruction_args(BuiltinAgent::Codex, text.into());
        let value: toml::Value = toml::from_str(args[1].to_str().unwrap()).unwrap();
        assert_eq!(value["developer_instructions"].as_str(), Some(text));
    }
}
