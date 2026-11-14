//! The gate's prompt: an opening each session of a space begins with, the
//! same for every one so a new session reuses its cache, then the request,
//! of which a resumed session is given only what is new.
use std::fmt::Write as _;

use crate::config::gate::GateRules;

/// What the gate is told before anything else.
const INSTRUCTIONS: &str = include_str!("instructions.md");

/// The most of one item's description the gate reads.
const BODY_LIMIT: usize = 4000;

/// The most of the commit log the gate reads.
const LOG_LIMIT: usize = 16_000;

/// The space a request comes from, as the gate's opening describes it.
pub(crate) struct Space<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub mode: Option<&'a str>,
    pub branch: Option<&'a str>,
    /// Its linked issues and PRs, each as [`item`] renders it.
    pub items: &'a [String],
}

/// The request, as the gate is asked to decide it.
pub(crate) struct Request<'a> {
    /// Its number and what approving it does.
    pub title: &'a str,
    pub target: &'a str,
    pub reason: &'a str,
    /// Its specification as JSON.
    pub specification: &'a str,
    /// `git log --stat` of the commits it brings, or those since the
    /// session's last verdict.
    pub commits: Option<&'a str>,
    /// What Shoal finds the action leaves out of the repository's
    /// conventions.
    pub warnings: &'a [String],
    /// The space's issues and PRs as they are now, for a session whose
    /// opening listed them as they were then.
    pub items: Option<&'a [String]>,
}

/// The opening of a session: the built-in instructions, the user's rules
/// and the space.
pub(crate) fn opening(rules: &GateRules, space: &Space) -> String {
    let mut text = INSTRUCTIONS.trim_end().to_owned();
    text.push_str("\n\n## The repository's rules\n\n");
    match rules.instructions.is_empty() {
        true => text.push_str("None beyond the instructions above.\n"),
        false => {
            for instruction in &rules.instructions {
                let _ = writeln!(text, "{}\n", instruction.trim_end());
            }
        }
    }
    text.push_str("\n## Cases you pass to the user\n\n");
    match rules.escalate.is_empty() {
        true => text.push_str("None: decide every request yourself.\n"),
        false => {
            for case in &rules.escalate {
                let _ = writeln!(text, "- {case}");
            }
        }
    }
    let _ = write!(text, "\n## The space\n\n{} {}", space.kind, space.name);
    if let Some(mode) = space.mode {
        let _ = write!(text, ", in mode {mode}");
    }
    if let Some(branch) = space.branch {
        let _ = write!(text, ", on branch {branch}");
    }
    text.push_str(".\n\n### Its issues and PRs\n\n");
    match space.items.is_empty() {
        true => text.push_str("None are linked.\n"),
        false => text.push_str(&space.items.join("\n")),
    }
    text
}

/// The request to decide, after the opening or in a resumed session.
pub(crate) fn request(request: &Request) -> String {
    let mut text = format!(
        "## Request {}\n\nTarget: {}\nThe agent's reason: {}\n\n```json\n{}\n```\n",
        request.title,
        request.target,
        match request.reason {
            "" => "none given",
            reason => reason,
        },
        request.specification
    );
    if let Some(commits) = request.commits {
        let commits = match commits.trim() {
            "" => "None.",
            commits => truncated(commits, LOG_LIMIT),
        };
        let _ = write!(text, "\n### Its commits\n\n```\n{commits}\n```\n");
    }
    if let Some(items) = request.items {
        text.push_str("\n### The space's issues and PRs now\n\n");
        match items.is_empty() {
            true => text.push_str("None are linked.\n"),
            false => text.push_str(&items.join("\n")),
        }
    }
    if !request.warnings.is_empty() {
        text.push_str("\n### What Shoal finds missing\n\n");
        for warning in request.warnings {
            let _ = writeln!(text, "- {warning}");
        }
    }
    text.push_str("\nDecide it, returning the verdict the output schema describes.\n");
    text
}

/// What a gate whose escalation quoted `quoted`, no listed case, is told.
pub(crate) fn misquoted(quoted: &str) -> String {
    format!(
        "Your escalation quoted {quoted:?}, which is not one of the cases listed for passing a \
         request to the user. Quote one of them word for word, or allow or deny the request."
    )
}

/// An issue or PR as the opening lists it: its number, title, state and
/// labels, its CI and review state for a PR, and its description.
pub(crate) fn item(
    relation: &str,
    details: &crate::forge::review::view::Details,
    url: &str,
) -> String {
    let mut text = format!(
        "#{} {} ({relation}, {}): {url}\nLabels: {}\n",
        details.number,
        details.title,
        details.state,
        match details.labels.is_empty() {
            true => "none".to_owned(),
            false => details.labels.join(", "),
        }
    );
    if let Some(pr) = &details.pr {
        let checks = (pr.checks.iter())
            .map(|check| format!("{}: {}", check.name, check.result))
            .collect::<Vec<_>>();
        let _ = writeln!(
            text,
            "Head {} into {}{}; checks: {}; review: {}",
            pr.head,
            pr.base,
            if pr.draft { ", draft" } else { "" },
            match checks.is_empty() {
                true => "none".to_owned(),
                false => checks.join(", "),
            },
            pr.review.map_or("none", |review| review.as_str())
        );
    }
    let _ = writeln!(text, "\n{}\n", truncated(details.body.trim(), BODY_LIMIT));
    text
}

/// `text` cut to at most `limit` bytes at a character boundary.
fn truncated(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
