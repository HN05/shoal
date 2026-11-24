//! A launch on a CI run, which names the run by its link, positionally or
//! with `--ci`: what it refuses beside the run, and the start the run
//! settles it into once read, from where the run ran: its PR's workspace,
//! its branch's, or a chat for a run on the default branch, a tag or no
//! branch.
use anyhow::{Result, bail, ensure};
use shoal_protocol::launches::{ItemChoice, Launch, StartTarget};
use shoal_protocol::space_names::SpaceName;

use crate::forge::{
    ci::run_action::Run,
    items::{
        link::LinkKind,
        reference::{ItemInput, ItemRef},
    },
    numbers::PullNumber,
};

/// Where a run's start goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStart {
    /// The workspace of the PR of its repository it ran for.
    Pull(PullNumber),
    /// The workspace of the branch it ran on.
    Branch(String),
    /// A chat: it ran on the default branch, a tag or no branch, which name
    /// no work of their own, or the start's mode starts a chat.
    Unbranched,
}

impl RunStart {
    /// Where `run` starts, given `branch`, the one it ran on when that is
    /// a branch of its repository other than the default branch.
    pub fn of(run: &Run, branch: Option<String>) -> Self {
        match (run.pull, branch) {
            (Some(pull), _) => Self::Pull(pull),
            (None, Some(branch)) => Self::Branch(branch),
            (None, None) => Self::Unbranched,
        }
    }

    /// The ref `run`, which ran for no PR, ran on where it is not the
    /// `default` branch: a branch, or a tag, which a forge reports in its
    /// place.
    pub fn other_ref<'a>(run: &'a Run, default: &str) -> Option<&'a str> {
        let branch = run.branch.as_deref().filter(|_| run.pull.is_none());
        branch.filter(|branch| *branch != default)
    }
}

/// Whether `launch` names a run, by its link or with `--ci`, picked or not.
pub fn names_run(launch: &Launch) -> bool {
    launch.ci.is_some() || matches!(launch.target, Some(StartTarget::Run(_)))
}

/// The run `launch` starts on, by its link, positionally or with `--ci`;
/// refused for `--ci` with a number, which names no run, or without a
/// value, which only a terminal picks.
pub fn named_run(launch: &Launch) -> Result<Option<&ItemRef>> {
    let flagged = match &launch.ci {
        None => None,
        Some(ItemChoice::Pick) => {
            bail!("--ci needs a CI run's URL; a terminal picks one without it")
        }
        Some(ItemChoice::Named(ItemInput::Number(_))) => {
            bail!("--ci takes a CI run's URL; a number names no run")
        }
        Some(ItemChoice::Named(ItemInput::Url(run))) => Some(run),
    };
    let positional = match &launch.target {
        Some(StartTarget::Run(run)) => Some(run),
        _ => None,
    };
    if let Some(run) = flagged {
        run.run()?;
    }
    Ok(flagged.or(positional))
}

/// The run a settled launch starts on, which it keeps as `--ci` names it.
pub fn settled_run(launch: &Launch) -> Option<&ItemRef> {
    match launch.ci.as_ref()?.named()? {
        ItemInput::Url(run) => Some(run),
        ItemInput::Number(_) => None,
    }
}

/// Refuse what a launch on a run takes beside it: a positional link beside
/// `--ci`, an issue, a PR, a branch or a copy, which name the work their own
/// way, and an existing workspace, which the run's start finds itself.
pub fn ensure_run_fits(launch: &Launch) -> Result<()> {
    if !names_run(launch) {
        return Ok(());
    }
    let linked =
        (launch.target.as_ref()).is_some_and(|target| target.linked_repository().is_some());
    ensure!(
        launch.ci.is_none() || !linked,
        "--ci cannot be combined with a positional link; name the work once"
    );
    let others = [
        ("--issue", launch.issue.is_some()),
        ("--pr", launch.pr.is_some()),
        ("--branch", launch.branch.is_some()),
        (
            "--copy",
            launch.copy.is_some() || launch.checkpoint.is_some(),
        ),
        ("--in", launch.in_workspace.is_some()),
    ];
    if let Some((flag, _)) = others.iter().find(|(_, set)| *set) {
        bail!("{flag} cannot be combined with a CI run, which says where the work is");
    }
    Ok(())
}

/// Settle `launch` on the run `url`, read as `run`, in the registered
/// repository `repository`, into the start `start` says: on its PR, on its
/// branch as `--branch` names it, or else without a branch, named after the
/// run unless the launch names it. The run says nothing of the mode, so a
/// launch that names none takes the default rather than one its PR's
/// labels call for. The launch keeps the run as `--ci` names it, for its
/// link and its agent's first message.
pub fn settle(
    launch: &mut Launch,
    (url, run): (ItemRef, &Run),
    start: RunStart,
    repository: String,
) -> Result<()> {
    launch.target = None;
    match start {
        RunStart::Pull(pull) => {
            let pull = ItemRef::numbered(url.repository(), LinkKind::Pr, pull.get())?;
            launch.target = Some(StartTarget::Pull(pull));
        }
        RunStart::Branch(branch) => launch.branch = Some(branch),
        RunStart::Unbranched => {
            let title = run.title(url.run()?);
            (launch.name).get_or_insert_with(|| SpaceName::derive(&title, "CI run"));
        }
    }
    launch.repository = Some(repository);
    let mode = launch
        .mode
        .take()
        .unwrap_or_else(|| super::default_mode(launch));
    launch.mode = Some(mode);
    launch.ci = Some(ItemChoice::Named(ItemInput::Url(url)));
    Ok(())
}

#[cfg(test)]
mod tests;
