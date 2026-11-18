//! What the agent of a space that changed since its session stopped is
//! told, and whether it starts from a fork of that session, for a space of
//! either kind: converted into the other kind, copied or restored while
//! another space holds its session, or moved where its session is not.
use shoal_refs::BranchName;

use super::{Copied, LastSession};
use crate::config::templates::{IssueFields, Subject};
use crate::model::Space;

/// How a space changed since its agent's session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// It became a space of the other kind.
    Converted,
    /// It is a copy of another space, or another space holds its session,
    /// as the [`Copied`] says.
    Copied(Copied),
    /// It moved where its session is not.
    Moved,
}

impl Change {
    /// How the space of `last` changed, as that session records it; `None`
    /// when it did not. A converted space is told so, whatever else it is.
    pub fn of(last: &LastSession) -> Option<Self> {
        if last.converted {
            return Some(Self::Converted);
        }
        if let Some(copied) = last.copied {
            return Some(Self::Copied(copied));
        }
        last.moved.then_some(Self::Moved)
    }

    /// Whether its agent may start from a fork of its session, which a
    /// moved space's is not here to give.
    pub fn forks(self) -> bool {
        !matches!(self, Self::Moved)
    }

    /// What the agent of `space` is told, carrying on from a fork of its
    /// session when `forked`, followed by `handoff`, before its mode's
    /// instructions, which take the fields of the issue `space` links.
    pub fn told(self, space: &Space, forked: bool, handoff: Option<&str>) -> Subject {
        let mut text = match self {
            Self::Converted => converted_prompt(space, forked),
            Self::Copied(copied) => copied_prompt(space, copied, forked),
            Self::Moved => moved_prompt(space),
        };
        if let Some(handoff) = handoff {
            text.push_str("\n\n");
            text.push_str(handoff);
        }
        Subject {
            text,
            issue: IssueFields::linked(&space.links),
        }
    }
}

/// What the agent of `space`, which became a space of its kind, is told of
/// its new place, carrying on from a fork of its session when `forked`.
fn converted_prompt(space: &Space, forked: bool) -> String {
    let path = space.workdir.path().display();
    let (mut prompt, before) = match space.worktree() {
        Some(worktree) => (
            format!(
                "Shoal converted this chat into a workspace. You now work in its worktree at {path} on branch {}.",
                worktree.branch().map(BranchName::as_str).unwrap_or("-"),
            ),
            "chat",
        ),
        None => (
            format!(
                "Shoal converted this workspace into a chat. You now work in the repository's checkout at {path}. The workspace's worktree is gone; its branch stays, and shoal restore brings its files back from the checkpoint Shoal kept."
            ),
            "workspace",
        ),
    };
    prompt.push_str(&if forked {
        format!(" This conversation continues the {before}'s.")
    } else {
        format!(
            " The {before}'s session is not available here: read the space's linked issue to see where the work stands."
        )
    });
    prompt
}

/// What the agent of `space`, copied from another or restored while
/// another space holds its session as `copied` says, is told, from a fork
/// of its session there when `forked`.
fn copied_prompt(space: &Space, copied: Copied, forked: bool) -> String {
    let mut prompt = match (copied.held_elsewhere, space.worktree()) {
        (true, _) => {
            "Shoal restored this space while another space holds its agent's session.".to_owned()
        }
        (false, Some(_)) => "Shoal copied this workspace from another onto a branch of its own; the original keeps running with its branch and PRs.".to_owned(),
        (false, None) => {
            "Shoal copied this chat from another; the original keeps running.".to_owned()
        }
    };
    prompt.push_str(if forked {
        " This conversation is a fork of its agent's, which continues there separately."
    } else {
        " Its agent's session is not available here."
    });
    if copied.earlier_files {
        prompt.push_str(" The files come from an earlier checkpoint than the conversation, which describes work they do not contain: check them before you act on what you remember.");
    }
    let read = match space.worktree() {
        Some(_) => "its linked issue, its commits and its uncommitted changes",
        None => "its linked issue",
    };
    prompt.push_str(&format!(
        " Continue working on the unfinished assignment: read {read} to see where it stands."
    ));
    prompt
}

/// What the new agent of `space`, which moved where its earlier session is
/// not available, is told.
fn moved_prompt(space: &Space) -> String {
    let read = match space.worktree() {
        Some(_) => "its linked issue and PRs, its commits and its uncommitted changes",
        None => "its linked issue",
    };
    format!(
        "Shoal moved this {}, and your earlier session is not available here. Continue working on the unfinished assignment: read {read} to see where it stands.",
        space.kind()
    )
}

#[cfg(test)]
mod tests;
