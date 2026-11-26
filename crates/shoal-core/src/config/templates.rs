//! Plain-text templates: substitute known fields once, never evaluate their values.
use std::{borrow::Cow, collections::BTreeMap};

use anyhow::{Context, Result};

use crate::{
    config::{Settings, modes},
    forge::{
        ci::{
            JobState,
            run_action::Run,
            run_view::{RunJob, RunView},
        },
        items::{issue::IssueDetails, reference::ItemRef},
    },
    model::{ChatHome, Head, SpaceKind, SpaceLinks, Workspace},
};

pub(crate) use crate::placeholders::render;

const WORKSPACE_FACTS: &str = include_str!("templates/facts/workspace.md");
const COMMIT_WORKSPACE_FACTS: &str = include_str!("templates/facts/commit_workspace.md");
const CHECKOUT_CHAT_FACTS: &str = include_str!("templates/facts/checkout_chat.md");
/// What the agent of a chat in a workspace's worktree is told about its
/// space, which [`CHECKOUT_CHAT_FACTS`] would describe as the registered
/// checkout.
const WORKSPACE_CHAT_FACTS: &str = include_str!("templates/facts/workspace_chat.md");
const ROOT_CHAT_FACTS: &str = include_str!("templates/facts/root_chat.md");
/// The practices Shoal's own mechanics depend on, sent to every agent after
/// its space's facts.
const SPACE_RULES: &str = include_str!("templates/space_rules.md");
/// What the agent of a chat in a repository's checkout whose mode works in a
/// workspace is told after the space rules, as a start without a branch
/// begins.
const CONVERT_RULE: &str = include_str!("templates/convert_rule.md");
/// A review's instructions: the fixed task, read-only rules, the commands
/// that write the review and treatment of history, then the base the
/// `review` rule replaces.
const REVIEW: &str = include_str!("templates/review.md");
const REVIEW_BASE_MARKER: &str = "<!-- base -->\n";

/// Whether a rule's text repeats the opening of the facts of a space, as a
/// copy of an earlier worker template does, although every launch already
/// receives them.
pub(crate) fn repeats_guidance(template: &str) -> bool {
    let openings = [
        WORKSPACE_FACTS,
        COMMIT_WORKSPACE_FACTS,
        CHECKOUT_CHAT_FACTS,
        WORKSPACE_CHAT_FACTS,
        ROOT_CHAT_FACTS,
    ]
    .map(|facts| facts.lines().next().unwrap_or_default());
    template.lines().any(|line| openings.contains(&line.trim()))
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
pub(crate) fn review_base() -> &'static str {
    REVIEW
        .split_once(REVIEW_BASE_MARKER)
        .map_or("", |(_, base)| base)
}

pub struct ReviewFields<'a> {
    pub workspace: &'a str,
    pub branch: &'a str,
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
    fn description(self) -> &'static str {
        match self {
            Self::Pushed => {
                "Shoal delivers the comments, reviews, CI results and merge conflicts of \
                 watched PRs to you as context after tool calls, and starts your next turn \
                 with them after you end one."
            }
            Self::Watched => {
                "`shoal watch pr` waits for the comments, reviews, CI results and merge \
                 conflicts of watched PRs and reports them; a timeout means no update."
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
    /// A chat about the repository named `repo`, or without one, run in
    /// `home`, which receives its updates by `delivery`.
    Chat {
        repo: Option<&'a str>,
        home: ChatHome<'a>,
        delivery: Delivery,
    },
}

impl Place<'_> {
    /// The mode `name` of an agent here, as [`modes::resolve`] finds it for
    /// this kind of space.
    fn mode<'a>(
        self,
        settings: &'a Settings,
        name: &modes::ModeName,
    ) -> Result<Cow<'a, modes::Mode>> {
        let (kind, repository) = match self {
            Self::Workspace { .. } => (SpaceKind::Workspace, true),
            Self::Chat { repo, .. } => (SpaceKind::Chat, repo.is_some()),
        };
        modes::resolve(&settings.modes, name, kind, repository)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Subject {
    /// The subject as the first message states it, the same in every mode.
    pub text: String,
    /// The issue it names, for the `{issue.*}` fields.
    pub(crate) issue: Option<IssueFields>,
}

impl Subject {
    pub fn told(text: String) -> Self {
        Self { text, issue: None }
    }

    /// `issue` as an agent's subject, worded the same in every mode; the
    /// body is the issue's description when the agent starts.
    pub fn issue(issue: &IssueDetails) -> Self {
        let number = issue.number.get();
        let mut text = format!("Issue #{number}: {}\n{}\n", issue.title, issue.url);
        let details = issue.details.trim();
        if !details.is_empty() {
            text.push_str(&format!("\nIssue description:\n{details}\n"));
        }
        text.push_str(&format!(
            "\n{}`shoal view` shows the issue with its comments and later edits.",
            comment_count(issue.comments)
        ));
        Self {
            text,
            issue: Some(IssueFields {
                number,
                title: issue.title.clone(),
                url: issue.url.to_string(),
            }),
        }
    }

    /// The CI run `run` names, read as `view`, as an agent's subject: its
    /// title, URL, commit and state, and its failed jobs, whose logs
    /// `shoal view ci` shows.
    pub fn run(run: &ItemRef, view: &RunView) -> Result<Self> {
        let read = &view.run;
        let mut text = format!("CI run: {}\n{run}\n", read.title(run.run()?));
        if let Some(commit) = &read.commit {
            text.push_str(&format!("Commit: {commit}\n"));
        }
        text.push_str(&run_state(read));
        let failed = failed_jobs(&view.jobs);
        if !failed.is_empty() {
            text.push_str(&format!("Failed jobs: {}\n", failed.join(", ")));
        }
        text.push_str(&format!(
            "\n`shoal view ci {run}` shows its jobs with the logs of the failed ones."
        ));
        Ok(Self::told(text))
    }
}

/// The line of a run's subject that states `run`'s state, with the result
/// it finished with once it finished.
fn run_state(run: &Run) -> String {
    match run.result {
        Some(result) if !run.state.unfinished() => format!("State: {} ({result})\n", run.state),
        _ => format!("State: {}\n", run.state),
    }
}

fn failed_jobs(jobs: &[RunJob]) -> Vec<&str> {
    (jobs.iter())
        .filter(|job| job.state == JobState::Failed)
        .map(|job| job.name.as_str())
        .collect()
}

/// What an agent's first message says of how many `comments` the issue
/// has, where the forge says: Shoal reports only those posted later.
fn comment_count(comments: Option<u64>) -> String {
    match comments {
        Some(0) => "The issue has no comments. ".to_owned(),
        Some(1) => "The issue has 1 comment. ".to_owned(),
        Some(count) => format!("The issue has {count} comments. "),
        None => String::new(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IssueFields {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) url: String,
}

impl IssueFields {
    /// The fields of the issue `links` assign, for an agent restored
    /// without its subject.
    pub(crate) fn linked(links: &SpaceLinks) -> Option<Self> {
        let issue = links.issue()?;
        Some(Self::at(&issue.url, issue.title.clone()))
    }

    /// The fields of `issue`, titled `title`, without asking the forge.
    pub(crate) fn at(issue: &ItemRef, title: Option<String>) -> Self {
        Self {
            number: issue.number().get(),
            title: title.unwrap_or_default(),
            url: issue.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The facts of the agent's space, the space rules, then the mode's
    /// rules.
    pub system: String,
    /// The subject, then the mode's instructions; `None` without a subject,
    /// as for a restored session, which has it already.
    pub first_message: Option<String>,
}

/// What an agent in `mode` at `place` is told, starting on `subject`.
pub fn brief(
    settings: &Settings,
    mode: &modes::ModeName,
    place: Place<'_>,
    subject: Option<&Subject>,
) -> Result<Brief> {
    let resolved = place.mode(settings, mode)?;
    let fields = Fields::new(
        (settings, mode),
        place,
        subject.and_then(|subject| subject.issue.as_ref()),
    );
    // The subject's text is the forge's or the user's, never a template.
    let first_message =
        subject.map(|subject| join([subject.text.clone(), fields.render(&resolved.instructions)]));
    Ok(Brief {
        system: system_text(settings, mode, place, &fields)?,
        first_message,
    })
}

/// The system text of an agent in `mode` at `place` on `issue`, as an
/// agent that already has its first message, such as a restored one, takes
/// it again.
pub(crate) fn system(
    settings: &Settings,
    mode: &modes::ModeName,
    place: Place<'_>,
    issue: Option<&IssueFields>,
) -> Result<String> {
    let fields = Fields::new((settings, mode), place, issue);
    system_text(settings, mode, place, &fields)
}

/// The facts of `place`, the space rules, then the rules of `mode`, with
/// `fields` filled in.
fn system_text(
    settings: &Settings,
    mode: &modes::ModeName,
    place: Place<'_>,
    fields: &Fields,
) -> Result<String> {
    let resolved = place.mode(settings, mode)?;
    let mut system = vec![facts(place).to_owned(), SPACE_RULES.to_owned()];
    if converts_to_workspace(place, &resolved) {
        system.push(CONVERT_RULE.to_owned());
    }
    for name in &resolved.rules {
        let rule = settings
            .rules
            .get(name)
            .with_context(|| format!("modes.{mode} names rule {name}, which no config defines"))?;
        system.push(rule.text.clone());
    }
    Ok(fields.render(&join(system)))
}

/// Whether the agent at `place` in `mode` is a chat in a repository's
/// checkout whose mode works in a workspace, which it becomes once the work
/// is known.
fn converts_to_workspace(place: Place<'_>, mode: &modes::Mode) -> bool {
    let in_checkout = matches!(
        place,
        Place::Chat {
            home: ChatHome::Checkout(_),
            ..
        }
    );
    in_checkout && mode.kind == SpaceKind::Workspace
}

fn facts(place: Place<'_>) -> &'static str {
    match place {
        Place::Workspace { workspace, .. } => match workspace.head {
            Head::Branch(_) => WORKSPACE_FACTS,
            Head::Commit(_) => COMMIT_WORKSPACE_FACTS,
        },
        Place::Chat { home, .. } => match home {
            ChatHome::Checkout(_) => CHECKOUT_CHAT_FACTS,
            ChatHome::Workspace(_) => WORKSPACE_CHAT_FACTS,
            ChatHome::Root(_) => ROOT_CHAT_FACTS,
        },
    }
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

struct Fields(Vec<(&'static str, String)>);

impl Fields {
    fn new(
        (settings, mode): (&Settings, &modes::ModeName),
        place: Place<'_>,
        issue: Option<&IssueFields>,
    ) -> Self {
        let mut fields = match place {
            Place::Workspace {
                workspace,
                delivery,
            } => vec![
                ("{workspace}", workspace.directory_name().to_owned()),
                (
                    "{branch}",
                    workspace
                        .branch()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                ),
                ("{head}", head_label(workspace)),
                ("{path}", workspace.path.to_string_lossy().into_owned()),
                ("{updates}", delivery.description().to_owned()),
            ],
            Place::Chat {
                repo,
                home,
                delivery,
            } => {
                let mut fields = vec![
                    ("{path}", home.path().to_string_lossy().into_owned()),
                    ("{updates}", delivery.description().to_owned()),
                ];
                fields.extend(repo.map(|repo| ("{repo}", repo.to_owned())));
                if let Some(workspace) = home.workspace() {
                    fields.extend([
                        ("{workspace}", workspace.directory_name().to_owned()),
                        // What selects the workspace, which its directory's
                        // name does not.
                        ("{workspace_number}", workspace.number.to_string()),
                        (
                            "{branch}",
                            workspace
                                .branch()
                                .map(ToString::to_string)
                                .unwrap_or_default(),
                        ),
                        ("{head}", head_label(workspace)),
                    ]);
                }
                fields
            }
        };
        if let Some(issue) = issue {
            fields.extend([
                ("{issue.number}", issue.number.to_string()),
                ("{issue.title}", issue.title.clone()),
                ("{issue.url}", issue.url.clone()),
            ]);
        }
        fields.push(("{relabel}", relabel(&settings.modes, mode)));
        Self(fields)
    }

    fn render(&self, text: &str) -> String {
        let fields: Vec<_> = (self.0.iter())
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        render(text, &fields)
    }
}

/// What `workspace` has checked out, as its agent's text, or that of a
/// chat in its worktree, names it.
fn head_label(workspace: &Workspace) -> String {
    match &workspace.head {
        Head::Branch(branch) => format!("on branch {branch}"),
        Head::Commit(at) => format!("at commit {}", at.label()),
    }
}

/// The sentence that has an agent in `mode`, once it settles an issue's
/// plan, swap the label of its own mode among `modes` for that of the mode
/// that carries the plan out; empty while no mode has a label.
fn relabel(modes: &BTreeMap<String, modes::Mode>, mode: &modes::ModeName) -> String {
    let own = modes
        .get(mode.as_str())
        .and_then(|mode| mode.label.as_deref());
    let others: Vec<_> = (modes.iter())
        .filter(|(name, _)| *name != mode.as_str())
        .filter_map(|(name, mode)| Some(format!("`{}` for `{name}`", mode.label.as_deref()?)))
        .collect();
    let carrying = format!(
        "the label of the mode that should carry out the plan: {}",
        others.join(", ")
    );
    match (own, others.is_empty()) {
        (None, true) => String::new(),
        (Some(own), true) => format!(" Then remove its `{own}` label."),
        (Some(own), false) => format!(" Then replace its `{own}` label with {carrying}."),
        (None, false) => format!(" Then add {carrying}."),
    }
}

#[cfg(test)]
mod tests;
