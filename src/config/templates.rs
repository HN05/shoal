//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};

use crate::{agent::BuiltinAgent, config::Config, paths::Paths};

pub use super::placeholders::render;

pub const ISSUE_FILE: &str = "issue-template.md";
pub const ISSUE_DEFAULT: &str = include_str!("templates/issue.md");
pub const AGENT_FILE: &str = "agent-template.md";
/// Sent to every agent launch ahead of the user's `agent_template` additions.
pub const AGENT_GUIDANCE: &str = include_str!("templates/agent.md");
pub const CHAT_FILE: &str = "chat-template.md";
pub const CHAT_DEFAULT: &str = include_str!("templates/chat.md");

pub fn read(directory: &Path, name: &str) -> Result<Option<String>> {
    let path = directory.join(name);
    crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))
}

pub fn install(paths: &Paths) -> Result<()> {
    let config = Config::path(paths);
    install_at(config.parent().context("config has no directory")?)
}

/// Seed missing templates; an existing file belongs to the user.
fn install_at(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    for (name, contents) in [(ISSUE_FILE, ISSUE_DEFAULT), (CHAT_FILE, CHAT_DEFAULT)] {
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

/// How a launched agent receives its issue and PR activity, which
/// `{updates}` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Hooks deliver it during a turn and start the next turn with it.
    Pushed,
    /// The agent watches for it.
    Watched,
}

impl Delivery {
    fn instruction(self) -> &'static str {
        match self {
            Self::Pushed => {
                "Shoal delivers their comments, reviews, CI results and merge\n   \
                 conflicts to you as context after tool calls, and starts your next turn\n   \
                 with them after you end one. Handle each delivery, then end your turn\n   \
                 to wait for the next."
            }
            Self::Watched => {
                "run `shoal watch pr`. Handle the reported\n   \
                 comments, reviews, CI results and merge conflicts, then wait again; a\n   \
                 timeout means no update."
            }
        }
    }
}

/// Shoal's guidance followed by the user's additions, which cannot replace it.
pub fn instructions(
    additions: Option<&str>,
    workspace: &crate::model::Workspace,
    delivery: Delivery,
) -> String {
    let guidance = AGENT_GUIDANCE.trim_end();
    let template = match additions {
        Some(additions) if !additions.is_empty() => format!("{guidance}\n\n{additions}"),
        _ => guidance.to_owned(),
    };
    render(
        &template,
        &[
            ("{workspace}", &workspace.name),
            ("{branch}", &workspace.branch),
            ("{path}", &workspace.path.to_string_lossy()),
            ("{updates}", delivery.instruction()),
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
        let chat = directory.path().join(CHAT_FILE);
        assert_eq!(fs::read_to_string(chat).unwrap(), CHAT_DEFAULT);
        assert!(!directory.path().join(AGENT_FILE).exists());
        fs::write(&path, "custom").unwrap();
        install_at(directory.path()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "custom");
    }

    #[test]
    fn the_guidance_tells_each_launch_how_it_receives_updates() {
        let workspace = workspace();
        let pushed = instructions(None, &workspace, Delivery::Pushed);
        assert!(
            pushed.contains("end your turn\n   to wait for the next."),
            "{pushed}"
        );
        assert!(!pushed.contains("shoal watch pr"), "{pushed}");
        let watched = instructions(None, &workspace, Delivery::Watched);
        assert!(watched.contains("open, run `shoal watch pr`."), "{watched}");
        assert!(!watched.contains("{updates}"));
    }

    #[test]
    fn additions_follow_the_current_guidance() {
        let workspace = workspace();
        let guidance = instructions(None, &workspace, Delivery::Watched);
        assert!(guidance.starts_with("You are working in Shoal workspace topic"));
        assert_eq!(
            instructions(Some(""), &workspace, Delivery::Watched),
            guidance
        );
        assert_eq!(
            instructions(Some("Babysit {branch}."), &workspace, Delivery::Watched),
            format!("{guidance}\n\nBabysit topic.")
        );
    }

    fn workspace() -> crate::model::Workspace {
        serde_json::from_value(serde_json::json!({
            "id": "id", "repository_id": "repo", "name": "topic", "branch": "topic",
            "path": "/work/topic", "state": "ready", "created_at": 0, "updated_at": 0,
        }))
        .unwrap()
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
