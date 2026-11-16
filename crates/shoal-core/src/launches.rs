//! What a [`Launch`] decides without reading Shoal's state: the kind of
//! space its arguments fit, the mode its issue's labels call for, and the
//! arguments a kind of space or another argument refuses, which a start
//! checks before it asks or creates anything.
use anyhow::{Result, ensure};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};
use shoal_refs::BranchName;

use crate::{
    config::{self, modes::ModeName},
    forge::items::reference::{ItemInput, ItemRef},
    model::{SpaceKind, Workspace},
};

/// Refuse the arguments the kind of space `mode`, or the other arguments,
/// start in cannot use, beside `unfit`, the flags only a workspace takes
/// that the caller read itself, before anything is asked or created.
pub fn ensure_fits(
    launch: &Launch,
    unfit: &[&'static str],
    kind: SpaceKind,
    mode: Option<&ModeName>,
) -> Result<()> {
    // A workspace takes every argument a chat does, but an existing one
    // none that names what a new space starts as.
    if kind == SpaceKind::Workspace {
        return ensure_worker_fits(launch);
    }
    let flags = workspace_flags(launch);
    let hint = "start it in a mode that starts in a workspace, such as -m worker";
    let Some(flag) = flags.first().or(unfit.first()) else {
        return Ok(());
    };
    match mode {
        Some(mode) => {
            anyhow::bail!("{flag} does not apply to a {kind}, which mode {mode} starts in; {hint}")
        }
        None => anyhow::bail!("{flag} does not apply to a {kind}; {hint}"),
    }
}

/// Refuse what a start in the existing workspace `--in` names takes in no
/// role, before its picker opens: a mode no start takes, a repository,
/// which is the workspace's, or what the worker mode, which always starts
/// the workspace's own agent, takes only for a new space.
pub fn ensure_in_fits(launch: &Launch) -> Result<()> {
    if launch.in_workspace.is_none() {
        return Ok(());
    }
    if let Some(mode) = &launch.mode {
        config::modes::ensure_startable(mode)?;
    }
    if launch.mode == Some(ModeName::DEFAULT) && !launch.chat {
        ensure_worker_fits(launch)?;
    }
    let repository =
        launch.repository.is_some() || matches!(launch.target, Some(StartTarget::Repository(_)));
    ensure!(
        !repository,
        "a start with --in starts in the workspace's repository; drop the repository"
    );
    Ok(())
}

/// Refuse what only a new space takes in a start of the worker of the
/// existing workspace `--in` names, picked or yet to be.
fn ensure_worker_fits(launch: &Launch) -> Result<()> {
    let Some(workspace) = &launch.in_workspace else {
        return Ok(());
    };
    let workspace =
        (workspace.named()).map_or_else(|| "<workspace>".to_owned(), ToString::to_string);
    if let Some(IssueChoice::Named(issue)) = &launch.issue {
        anyhow::bail!(
            "--issue names a new space's issue; link it to the workspace with `shoal link issue {issue} assigned --space {workspace}`"
        );
    }
    let new_space = [
        ("<repository>", launch.target.is_some()),
        ("--repo", launch.repository.is_some()),
        ("--issue", launch.issue.is_some()),
        ("--parent", launch.parent.is_some()),
        ("--name", launch.name.is_some()),
        // `shoal priority` sets an existing workspace's.
        ("--priority", launch.priority.is_some()),
    ];
    if let Some((flag, _)) = new_space.iter().find(|(_, set)| *set) {
        anyhow::bail!(
            "{flag} applies to a new space, and --in starts the worker of an existing workspace"
        );
    }
    Ok(())
}

/// The flags the launch sets that only a workspace takes.
pub fn workspace_flags(launch: &Launch) -> Vec<&'static str> {
    [
        ("--branch", launch.branch.is_some()),
        ("--path", launch.path.is_some()),
        ("--base", launch.base.is_some()),
        ("--git-profile", launch.git_profile.is_some()),
        ("--fork", launch.fork.is_some()),
        ("--copy", launch.copy.is_some()),
        ("--checkpoint", launch.checkpoint.is_some()),
        ("--add-blocker", !launch.blockers.is_empty()),
        ("--skip-setup", launch.skip_setup),
        ("--disposable", launch.disposable),
    ]
    .into_iter()
    .filter_map(|(flag, set)| set.then_some(flag))
    .collect()
}

/// The kind of space the arguments other than the mode fit: a workspace
/// for a flag only a workspace takes or a PR or branch link.
pub fn fitting_kind(launch: &Launch) -> Option<SpaceKind> {
    (links_branch(launch) || !workspace_flags(launch).is_empty()).then_some(SpaceKind::Workspace)
}

/// Whether the positional argument links a PR or branch, which only a
/// workspace starts on.
fn links_branch(launch: &Launch) -> bool {
    matches!(
        launch.target,
        Some(StartTarget::Pull(_) | StartTarget::Branch { .. })
    )
}

/// Refuse a copy beside a PR or branch link, which would open that branch
/// instead, before anything is asked or looked up.
pub fn ensure_copy_fits(launch: &Launch) -> Result<()> {
    let copies = launch.copy.is_some() || launch.checkpoint.is_some();
    ensure!(
        !copies || !links_branch(launch),
        "a copy starts from the workspace it copies, not a PR or branch link"
    );
    Ok(())
}

/// The positional argument once the launch says whether a number names an
/// issue: beside a branch or issue of its own, it names a repository.
pub fn decided_target(launch: &Launch) -> Result<Option<StartTarget>> {
    let names_branch = launch.branch.is_some() || launch.issue.is_some();
    (launch.target.clone())
        .map(|target| target.decided(names_branch))
        .transpose()
}

/// The mode without one named: setup for a repository's setup, chat for a
/// start that asks for a chat, whose agent the worker mode would not tell
/// it is one, else the worker.
pub fn default_mode(launch: &Launch) -> ModeName {
    if launch.repository_setup {
        return ModeName::SETUP;
    }
    match launch.chat {
        true => ModeName::CHAT,
        false => ModeName::DEFAULT,
    }
}

/// Refuse `mode` for `launch` where it cannot start in it: the setup mode
/// outside a repository's setup, which starts in no other.
pub fn ensure_mode_fits(launch: &Launch, mode: &ModeName) -> Result<()> {
    if !launch.repository_setup {
        return config::modes::ensure_startable(mode);
    }
    ensure!(
        *mode == ModeName::SETUP,
        "a repository's setup starts in mode {}",
        ModeName::SETUP
    );
    Ok(())
}

/// The mode for an issue labelled `labels`: the worker once `[issue]
/// planned_label` says its plan is settled, else the plan mode while
/// `plan_label` says it needs one.
pub fn labelled_mode(config: &config::Issue, labels: &[String]) -> Option<ModeName> {
    let labelled =
        |label: &Option<String>| label.as_ref().is_some_and(|label| labels.contains(label));
    if labelled(&config.planned_label) {
        Some(ModeName::DEFAULT)
    } else if labelled(&config.plan_label) {
        Some(ModeName::PLAN)
    } else {
        None
    }
}

/// The issue `--issue` or the positional argument names by number or URL.
pub fn named_issue(launch: &Launch) -> Result<Option<ItemInput>> {
    Ok(match (&launch.issue, &launch.target) {
        (Some(IssueChoice::Named(issue)), _) => Some(issue.clone()),
        (_, Some(positional)) => match positional.clone().decided(false)? {
            StartTarget::Issue(issue) => Some(issue),
            _ => None,
        },
        _ => None,
    })
}

/// The repository the positional argument may name: a repository's, or a
/// number, which names a repository of that name until the start decides it
/// names an issue.
pub fn positional_repository(launch: &Launch) -> Option<&str> {
    match &launch.target {
        Some(StartTarget::Repository(named) | StartTarget::Number(named)) => Some(named),
        _ => None,
    }
}

/// The issue and PR the arguments name by URL.
pub fn linked_items(launch: &Launch) -> impl Iterator<Item = &ItemRef> {
    let issue = match &launch.issue {
        Some(IssueChoice::Named(ItemInput::Url(issue))) => Some(issue),
        _ => None,
    };
    let positional = match &launch.target {
        Some(StartTarget::Issue(ItemInput::Url(item)) | StartTarget::Pull(item)) => Some(item),
        _ => None,
    };
    issue.into_iter().chain(positional)
}

/// The workspace among `workspaces` on the local branch `name` selects: its
/// own name, or the branch of the `remote` one it names.
pub fn on_branch<'a>(
    workspaces: &'a [Workspace],
    name: &str,
    remote: Option<&(String, BranchName)>,
) -> Option<&'a Workspace> {
    let local = remote.map_or(name, |(_, branch)| branch.as_str());
    (workspaces.iter()).find(|workspace| workspace.branch().map(BranchName::as_str) == Some(local))
}

/// Refuse a repository or issue the launch names for `mode`, which starts a
/// chat without a repository.
pub fn ensure_no_repository(launch: &Launch, mode: &ModeName) -> Result<()> {
    let names = [
        ("<repository>", launch.target.is_some()),
        ("--repo", launch.repository.is_some()),
        ("--issue", launch.issue.is_some()),
    ];
    if let Some((flag, _)) = names.iter().find(|(_, set)| *set) {
        anyhow::bail!(
            "{flag} does not apply to mode {mode}, which starts a chat without a repository"
        );
    }
    Ok(())
}

/// The repository and issue a chat starts on, from the positional argument,
/// `--repo` and `--issue`: a positional issue number or URL is the issue, as
/// it is for a workspace, and a positional repository is the repository. A
/// PR or branch link, two of the same, or a repository beside `--in`, whose
/// workspace has one, is refused.
pub fn chat_target(
    positional: Option<StartTarget>,
    repository: Option<String>,
    issue: Option<IssueChoice>,
    in_workspace: bool,
) -> Result<(Option<String>, Option<IssueChoice>)> {
    let (repository, issue) = match positional {
        Some(positional) => positional_target(positional, repository, issue)?,
        None => (repository, issue),
    };
    ensure!(
        !in_workspace || repository.is_none(),
        "a chat with --in starts in the workspace's repository; drop the repository"
    );
    Ok((repository, issue))
}

/// [`chat_target`] with a `positional` argument, before the check against
/// `--in`.
fn positional_target(
    positional: StartTarget,
    repository: Option<String>,
    issue: Option<IssueChoice>,
) -> Result<(Option<String>, Option<IssueChoice>)> {
    // A chat takes a positional number as its issue.
    match positional.decided(false)? {
        StartTarget::Pull(_) | StartTarget::Branch { .. } => anyhow::bail!(
            "a chat starts on an issue, not a PR or branch link; start it in a mode that starts in a workspace, such as -m worker"
        ),
        StartTarget::Issue(positional) => {
            ensure!(
                issue.is_none(),
                "name the issue once, positionally or with --issue"
            );
            Ok((repository, Some(IssueChoice::Named(positional))))
        }
        StartTarget::Repository(positional) | StartTarget::Number(positional) => {
            ensure!(
                repository.is_none(),
                "a positional repository cannot be combined with --repo"
            );
            Ok((Some(positional), issue))
        }
    }
}

mod reads;
pub mod shape;
pub mod workspace;

pub use reads::LaunchReads;

#[cfg(test)]
mod tests;
