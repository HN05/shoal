//! What the new tool of a handoff is told before its mode's instructions:
//! what the space is, that it continues work another tool started, and
//! where the copies of the earlier sessions are.
use shoal_refs::BranchName;

use super::{Leaving, copies::Listed};
use crate::{agent::Agent, model::Space};

/// What `taking`, the agent taking the work of `space` over from
/// `leaving`, is told of the space and of the sessions `listed`, oldest
/// first.
pub fn told(space: &Space, leaving: Option<&Leaving>, taking: &Agent, listed: &[Listed]) -> String {
    let mut parts = vec![taken_over(space, leaving, taking)];
    if !listed.is_empty() {
        parts.push(sessions(space, leaving, listed));
    }
    parts.join("\n\n")
}

/// That `taking` continues the work of `space`, with what the space is and
/// what it works on.
fn taken_over(space: &Space, leaving: Option<&Leaving>, taking: &Agent) -> String {
    let kind = space.kind();
    let from = match leaving {
        Some(leaving) => format!(" that {} started", leaving.agent),
        None => String::new(),
    };
    let path = space.workdir.path().display();
    let place = match space.worktree() {
        Some(worktree) => format!(
            "Its worktree is at {path}, on branch {}.",
            worktree.branch().map(BranchName::as_str).unwrap_or("-")
        ),
        None => format!("It runs in {path}."),
    };
    let work = match space.links.issue() {
        Some(issue) => match &issue.title {
            Some(title) => format!(" It works on the issue \"{title}\", {}.", issue.url),
            None => format!(" It works on the issue {}.", issue.url),
        },
        None => String::new(),
    };
    format!(
        "Shoal handed the work of this {kind}, {}, over to you, {taking}: you continue work{from} in another agent tool. {place}{work}",
        space.name
    )
}

/// The sessions `listed`, oldest first, each with where its copy is, the
/// one `leaving` stopped marked to read first.
fn sessions(space: &Space, leaving: Option<&Leaving>, listed: &[Listed]) -> String {
    let stale = match space.worktree() {
        Some(_) => "the worktree",
        None => "the files",
    };
    let mut text = format!(
        "The earlier sessions are copied into this {}'s attached files as each tool wrote them, oldest first; what the tools printed in them may be stale against {stale} now.",
        space.kind()
    );
    let first = leaving.and_then(|leaving| leaving.session.as_ref());
    for session in listed {
        text.push_str("\n- ");
        text.push_str(&line(session, first == Some(&session.session)));
    }
    text
}

/// The line that lists `session`, marked to read first when `first`.
fn line(session: &Listed, first: bool) -> String {
    let mark = match first {
        true => " (the one just stopped: read it first)",
        false => "",
    };
    let name = format!("{} session {}{mark}", session.agent, session.session);
    if let Some(missing) = &session.missing {
        return format!("{name}: not copied, {missing}");
    }
    let others = match session.others {
        0 => String::new(),
        1 => ", with 1 more file of it that shoal files lists".to_owned(),
        others => format!(", with {others} more files of it that shoal files lists"),
    };
    match &session.transcript {
        Some(transcript) => format!("{name}: {}{others}", transcript.display()),
        None => format!("{name}: no conversation file{others}"),
    }
}

#[cfg(test)]
mod tests;
