//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};

use crate::{agent::BuiltinAgent, config::Config, paths::Paths};

pub use super::placeholders::render;

pub const ISSUE_FILE: &str = "issue-template.md";
pub const ISSUE_DEFAULT: &str = include_str!("../../issue-template.md");
pub const AGENT_FILE: &str = "agent-template.md";
pub const AGENT_DEFAULT: &str = include_str!("../../agent-template.md");

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
    for (name, contents) in [(ISSUE_FILE, ISSUE_DEFAULT), (AGENT_FILE, AGENT_DEFAULT)] {
        let path = directory.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(contents.as_bytes())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
        }
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
