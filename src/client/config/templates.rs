//! Plain-text templates: substitute known fields once, never evaluate their values.
use anyhow::{Context, Result};

use crate::{
    config::{Settings, modes},
    model::{ChatHome, Workspace},
};

pub use super::placeholders::render;

/// Sent to every workspace agent ahead of its mode's rules.
pub const AGENT_GUIDANCE: &str = include_str!("templates/agent.md");
/// What the `chat` rule says first for a chat in a repository's checkout.
pub const CHAT_DEFAULT: &str = include_str!("templates/chat.md");
/// What the `chat` rule says first for a chat in a workspace's worktree,
/// which [`CHAT_DEFAULT`] would describe as the registered checkout.
pub const WORKSPACE_CHAT: &str = include_str!("templates/workspace_chat.md");
/// A review's instructions: the fixed task, read-only rules, output contract
/// and treatment of history, then the base the `review` rule replaces.
const REVIEW: &str = include_str!("templates/review.md");
const REVIEW_BASE_MARKER: &str = "<!-- base -->\n";

/// Whether a rule's text repeats the opening of Shoal's guidance, as a copy
/// of an earlier worker template does, although every launch already
/// receives it.
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

/// The part of a review's instructions no layer replaces.
fn review_fixed() -> &'static str {
    REVIEW
        .split_once(REVIEW_BASE_MARKER)
        .map_or(REVIEW, |(fixed, _)| fixed)
}

/// The review style the `review` rule starts from.
pub fn review_base() -> &'static str {
    REVIEW
        .split_once(REVIEW_BASE_MARKER)
        .map_or("", |(_, base)| base)
}

/// What a review's instructions name.
pub struct ReviewFields<'a> {
    pub workspace: &'a str,
    pub branch: &'a str,
    /// The review's checkout.
    pub path: &'a str,
    /// The commit the changes are compared against.
    pub base: &'a str,
}

/// A review's instructions: the fixed part, then `template`, the `review`
/// rule's text.
pub fn review_instructions(template: Option<&str>, fields: &ReviewFields) -> String {
    let fixed = review_fixed().trim_end();
    let template = match template.map(str::trim_end) {
        Some(template) if !template.is_empty() => format!("{fixed}\n\n{template}"),
        _ => fixed.to_owned(),
    };
    render(
        &template,
        &[
            ("{workspace}", fields.workspace),
            ("{branch}", fields.branch),
            ("{path}", fields.path),
            ("{base}", fields.base),
        ],
    )
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

/// Where an agent runs, as its text names it.
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// The agent of `workspace`, which receives its updates by `delivery`.
    Workspace {
        workspace: &'a Workspace,
        delivery: Delivery,
    },
    /// A chat about the repository named `repo`, run in `home`.
    Chat { repo: &'a str, home: ChatHome<'a> },
}

/// What an agent starts on: an issue, or a task given in words.
#[derive(Debug, Clone, Default)]
pub struct Subject {
    /// The subject as the first message states it, the same in every mode.
    pub text: String,
    /// The issue it names, for the `{issue.*}` fields.
    pub issue: Option<IssueFields>,
}

impl Subject {
    /// A task given in words.
    pub fn told(text: String) -> Self {
        Self { text, issue: None }
    }
}

#[derive(Debug, Clone)]
pub struct IssueFields {
    pub number: u64,
    pub title: String,
    pub url: String,
}

/// An agent's system text and its first message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// Shoal's guidance for a workspace's agent, then the mode's rules.
    pub system: String,
    /// The subject, then the mode's instructions; `None` without a subject,
    /// as for a restored session, which has it already.
    pub first_message: Option<String>,
}

/// What an agent in `mode` at `place` is told, starting on `subject`.
pub fn brief(
    settings: &Settings,
    mode: &str,
    place: Place<'_>,
    subject: Option<&Subject>,
) -> Result<Brief> {
    let resolved = modes::get(&settings.modes, mode)?;
    let mut system = Vec::new();
    if let Place::Workspace { .. } = place {
        system.push(AGENT_GUIDANCE.trim_end().to_owned());
    }
    for name in &resolved.rules {
        let rule = settings
            .rules
            .get(name)
            .with_context(|| format!("modes.{mode} names rule {name}, which no config defines"))?;
        system.push(rule_text(name, &rule.text, place));
    }
    let fields = Fields::new(settings, place, subject);
    // The subject's text is the forge's or the user's, never a template.
    let first_message =
        subject.map(|subject| join([subject.text.clone(), fields.render(&resolved.instructions)]));
    Ok(Brief {
        system: fields.render(&join(system)),
        first_message,
    })
}

/// A rule's configured `text`, after Shoal's description of a chat's place
/// for the `chat` rule of a chat.
fn rule_text(name: &str, text: &str, place: Place<'_>) -> String {
    let bundled = match place {
        Place::Chat { home, .. } if name == modes::CHAT_RULE => match home {
            ChatHome::Checkout(_) => CHAT_DEFAULT,
            ChatHome::Workspace(_) => WORKSPACE_CHAT,
        },
        _ => "",
    };
    join([bundled.trim_end().to_owned(), text.to_owned()])
}

/// The nonempty `parts`, trimmed at their ends, a blank line apart.
fn join(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .map(|part| part.trim_end().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The placeholders an agent's text takes, with their values.
struct Fields(Vec<(&'static str, String)>);

impl Fields {
    fn new(settings: &Settings, place: Place<'_>, subject: Option<&Subject>) -> Self {
        let mut fields = match place {
            Place::Workspace {
                workspace,
                delivery,
            } => vec![
                ("{workspace}", workspace.name.clone()),
                ("{branch}", workspace.branch.clone().unwrap_or_default()),
                ("{path}", workspace.path.to_string_lossy().into_owned()),
                ("{updates}", delivery.instruction().to_owned()),
            ],
            Place::Chat { repo, home } => {
                let mut fields = vec![
                    ("{repo}", repo.to_owned()),
                    ("{path}", home.path().to_string_lossy().into_owned()),
                ];
                if let Some(workspace) = home.workspace() {
                    fields.extend([
                        ("{workspace}", workspace.name.clone()),
                        ("{branch}", workspace.branch.clone().unwrap_or_default()),
                        ("{head}", head_label(workspace)),
                    ]);
                }
                fields
            }
        };
        if let Some(issue) = subject.and_then(|subject| subject.issue.as_ref()) {
            fields.extend([
                ("{issue.number}", issue.number.to_string()),
                ("{issue.title}", issue.title.clone()),
                ("{issue.url}", issue.url.clone()),
            ]);
        }
        fields.push(("{relabel}", relabel(&settings.issue)));
        Self(fields)
    }

    fn render(&self, text: &str) -> String {
        let fields: Vec<_> = (self.0.iter())
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        render(text, &fields)
    }
}

/// What `workspace` has checked out, as a chat's text names it.
fn head_label(workspace: &Workspace) -> String {
    match (&workspace.branch, &workspace.at) {
        (Some(branch), _) => format!("on branch {branch}"),
        (None, Some(at)) => format!("at commit {}", at.label()),
        (None, None) => "on a detached HEAD".to_owned(),
    }
}

/// The sentence that swaps an issue's labels once its plan is settled.
fn relabel(labels: &crate::config::Issue) -> String {
    match (
        labels.plan_label.as_deref(),
        labels.planned_label.as_deref(),
    ) {
        (Some(plan), Some(planned)) => {
            format!(" Then replace its `{plan}` label with `{planned}`.")
        }
        (Some(plan), None) => format!(" Then remove its `{plan}` label."),
        (None, Some(planned)) => format!(" Then add the `{planned}` label."),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests;
