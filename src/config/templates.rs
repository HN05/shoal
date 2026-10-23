//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{config::Config, paths::Paths};

pub use super::placeholders::render;

/// Retired with issue templates, now that the issue prompt is built in; read
/// only to warn about instructions it still holds.
pub const RETIRED_ISSUE_FILE: &str = "issue-template.md";
/// What `install` used to seed as the issue template, which holds no
/// instructions of the user's.
const RETIRED_ISSUE_DEFAULT: &str =
    "Work on issue #{number}: {title}\n{url}\n\nIssue details:\n{body}";
pub const WORKER_FILE: &str = "worker-template.md";
/// What `worker-template.md` was called; still read, with a rename warning.
pub const RENAMED_AGENT_FILE: &str = "agent-template.md";
/// Sent to every agent launch ahead of the user's `worker_template` additions.
pub const AGENT_GUIDANCE: &str = include_str!("templates/agent.md");
pub const CHAT_FILE: &str = "chat-template.md";
pub const CHAT_DEFAULT: &str = include_str!("templates/chat.md");

pub fn read(directory: &Path, name: &str) -> Result<Option<String>> {
    let path = directory.join(name);
    crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))
}

/// Refuse one level setting the worker template under both its names.
pub fn ensure_one_worker(worker: &Option<String>, renamed: &Option<String>) -> Result<()> {
    ensure!(
        worker.is_none() || renamed.is_none(),
        "worker_template and its former name agent_template are both set; keep only worker_template"
    );
    Ok(())
}

/// Fill one level's omitted inline worker template from `worker-template.md`,
/// or the file's former name, in `directory`; both files at once is an error.
pub fn read_worker(
    directory: &Path,
    worker: &mut Option<String>,
    renamed: &mut Option<String>,
) -> Result<()> {
    if worker.is_some() || renamed.is_some() {
        return Ok(());
    }
    *worker = read(directory, WORKER_FILE)?;
    *renamed = read(directory, RENAMED_AGENT_FILE)?;
    ensure!(
        worker.is_none() || renamed.is_none(),
        "both {WORKER_FILE} and {RENAMED_AGENT_FILE} exist in {}; keep only {WORKER_FILE}",
        directory.display()
    );
    Ok(())
}

pub fn install(paths: &Paths) -> Result<()> {
    let config = Config::path(paths);
    install_at(config.parent().context("config has no directory")?)
}

/// Seed the missing chat template; an existing file belongs to the user.
fn install_at(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join(CHAT_FILE);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file.write_all(CHAT_DEFAULT.as_bytes())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    Ok(())
}

/// Whether a retired issue template holds instructions the user wrote.
pub fn is_custom_issue_template(template: &str) -> bool {
    let template = template.trim();
    !template.is_empty() && template != RETIRED_ISSUE_DEFAULT
}

/// Chat `instructions` sent as an agent's first message, which starts a turn
/// before the user speaks, so it asks the agent to wait for them.
pub fn chat_first_message(instructions: &str) -> Option<String> {
    let instructions = instructions.trim_end();
    (!instructions.is_empty()).then(|| {
        format!(
            "{instructions}\n\nReply only that you are ready, then wait for the user's first message."
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_seeds_the_chat_template_and_preserves_edits() {
        let directory = tempfile::tempdir().unwrap();
        install_at(directory.path()).unwrap();
        let path = directory.path().join(CHAT_FILE);
        assert_eq!(fs::read_to_string(&path).unwrap(), CHAT_DEFAULT);
        assert!(!directory.path().join(WORKER_FILE).exists());
        assert!(!directory.path().join(RETIRED_ISSUE_FILE).exists());
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

    #[test]
    fn only_an_edited_retired_issue_template_holds_instructions() {
        assert!(!is_custom_issue_template(&format!(
            "{RETIRED_ISSUE_DEFAULT}\n"
        )));
        assert!(is_custom_issue_template(&format!(
            "{RETIRED_ISSUE_DEFAULT}\n\nPlan first."
        )));
    }

    fn workspace() -> crate::model::Workspace {
        serde_json::from_value(serde_json::json!({
            "id": "id", "repository_id": "repo", "name": "topic", "branch": "topic",
            "path": "/work/topic", "state": "ready", "created_at": 0, "updated_at": 0,
        }))
        .unwrap()
    }
}
