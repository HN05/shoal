//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    config::{Config, repo::TemplatesConfig},
    paths::Paths,
};

pub use super::placeholders::render;

pub const WORKER_FILE: &str = "worker-template.md";
/// Sent to every agent launch ahead of the user's `agent.templates.worker` additions.
pub const AGENT_GUIDANCE: &str = include_str!("templates/agent.md");
pub const CHAT_FILE: &str = "chat-template.md";
pub const CHAT_DEFAULT: &str = include_str!("templates/chat.md");

/// The Markdown file each of one layer's templates came from, by option key.
pub type Files = BTreeMap<String, PathBuf>;

/// Fill one layer's omitted inline templates from the Markdown files in
/// `directories`, where each file may be in only one of them.
pub fn fill(templates: &mut TemplatesConfig, directories: &[PathBuf]) -> Result<Files> {
    let mut files = Files::new();
    for (key, name, value) in [
        ("agent.templates.worker", WORKER_FILE, &mut templates.worker),
        ("agent.templates.chat", CHAT_FILE, &mut templates.chat),
    ] {
        let found = find(directories, name)?;
        if let (None, Some((path, text))) = (&value, found) {
            *value = Some(text);
            files.insert(key.to_owned(), path);
        }
    }
    Ok(files)
}

/// The one file named `name` among `directories`, with its text.
fn find(directories: &[PathBuf], name: &str) -> Result<Option<(PathBuf, String)>> {
    let mut found = Vec::new();
    for path in directories.iter().map(|directory| directory.join(name)) {
        let text = crate::fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?;
        found.extend(text.map(|text| (path, text)));
    }
    if let [(first, _), (second, _), ..] = found.as_slice() {
        bail!(
            "both {} and {} exist; keep only one {name}",
            first.display(),
            second.display()
        );
    }
    Ok(found.pop())
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

/// Whether a worker template repeats the opening of Shoal's guidance, as a
/// copy of an earlier default does, although every launch already receives it.
pub fn repeats_guidance(template: &str) -> bool {
    let opening = AGENT_GUIDANCE.lines().next().unwrap_or_default();
    template.lines().any(|line| line.trim() == opening)
}

/// Chat `instructions` sent as an agent's first message, which starts a turn
/// before the user speaks: followed by the chat's `opening` task when it has
/// one, otherwise by a request to wait for the user.
pub fn chat_first_message(instructions: &str, opening: Option<&str>) -> Option<String> {
    let instructions = instructions.trim_end();
    let next =
        opening.unwrap_or("Reply only that you are ready, then wait for the user's first message.");
    match (instructions.is_empty(), opening) {
        (true, None) => None,
        (true, Some(opening)) => Some(opening.to_owned()),
        (false, _) => Some(format!("{instructions}\n\n{next}")),
    }
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
                "Shoal delivers their comments, reviews, CI results and\n\
                 merge conflicts to you as context after tool calls, and starts your next\n\
                 turn with them after you end one. Handle each delivery, then end your\n\
                 turn to wait for the next."
            }
            Self::Watched => {
                "run `shoal watch pr`. Handle the reported\n\
                 comments, reviews, CI results and merge conflicts, then wait again; a\n\
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
mod tests;
