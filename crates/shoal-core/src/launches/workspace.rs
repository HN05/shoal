//! What a launch that creates or reopens a workspace starts on: the branch
//! it names as typed, a PR's or branch link's branch, or the branch earlier
//! work on its issue left, which the CLI and the daemon settle the same way
//! from what [`LaunchReads`] reads.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_refs::BranchName;

use super::LaunchReads;
use crate::{
    forge::{ForgeRepo, PullRequest, items::reference::ItemRef},
    git,
    model::{Repository, Workspace},
    protocol::PullHead,
    start_plan::{CopyCreation, Creation},
    state::WorkspaceState,
};

/// The longest branch an issue names, and the most title words it takes.
const BRANCH_NAME_LEN: usize = 32;
const BRANCH_NAME_WORDS: usize = 3;
/// Title words that add length without naming the issue.
const FILLER_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "its", "of",
    "on", "or", "s", "that", "the", "this", "to", "when", "with",
];

/// The branch of issue `number` titled `title`, `<number>-<words>`: up to
/// three title words, or `issue-<number>` when the title has none, so a
/// bare number is never mistaken for a revision.
pub fn issue_branch(number: u64, title: &str) -> String {
    let words = title_words(title);
    if words.is_empty() {
        return format!("issue-{number}");
    }
    let mut name = number.to_string();
    for (index, word) in words.iter().enumerate() {
        let room = BRANCH_NAME_LEN.saturating_sub(name.len() + 1);
        if word.len() > room {
            if index == 0 {
                name.push('-');
                name.push_str(&word[..room]);
            }
            break;
        }
        name.push('-');
        name.push_str(word);
    }
    name
}

/// The first lowercase ASCII words of `title` that name the issue.
fn title_words(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|word| !FILLER_WORDS.contains(&word.as_str()))
        .take(BRANCH_NAME_WORDS)
        .collect()
}

/// Refuse a link that is not of `repo`, the repository of its checkout's
/// origin.
pub async fn ensure_link_in(repo: &Repository, linked: Option<&ForgeRepo>) -> Result<()> {
    let remote = crate::repository::remote_url_from_path(&repo.path)
        .await?
        .context("forge lookup needs an origin remote")?;
    ensure!(
        linked.is_some_and(|linked| linked.is_remote(&remote)),
        "link belongs to a different repository"
    );
    Ok(())
}

/// What `launch` creates as it names it, a copy of `copy` when it copies
/// one, before its target settles the rest.
pub fn launch_creation(
    launch: &shoal_protocol::launches::Launch,
    copy: Option<CopyCreation>,
) -> Creation {
    Creation {
        path: launch.path.clone(),
        branch: launch.branch.clone(),
        base: launch.base.clone(),
        git_profile: launch.git_profile.clone(),
        options: crate::protocol::CreationOptions {
            name: launch.name.clone(),
            priority: launch.priority,
            skip_setup: launch.skip_setup,
            disposable: launch.disposable,
        },
        copy,
        ..Creation::default()
    }
}

/// Open the pushed `branch` a branch link names.
pub async fn branch_creation(branch: &str, creation: &mut Creation) -> Result<()> {
    git::check_branch_name(branch)?;
    creation.existing = Some(git::remote_ref("origin", branch));
    Ok(())
}

/// Reopen the workspace that owns the PR's head branch, or open the pushed
/// branch compared against the PR's base; either way the PR is linked.
pub async fn pull_creation(
    reads: &impl LaunchReads,
    repo: &Repository,
    pull: PullRequest,
    creation: &mut Creation,
) -> Result<()> {
    // Without a name of its own, a new workspace takes the PR's title.
    (creation.options.name).get_or_insert_with(|| {
        shoal_protocol::space_names::SpaceName::derive(&pull.title, "workspace")
    });
    if pull.fork {
        ensure_fork_opens(&pull, creation)?;
        creation.pull_head = Some(fork_head(repo, &pull).await?);
        creation.pr = Some(pull.url.into());
        return Ok(());
    }
    let owned = pull_head_settled(reads, repo, &pull, creation).await?;
    if !owned && creation.base.is_none() {
        let base = pull.base.as_str();
        creation.base = Some(fetch_pull_branch(&repo.path, "base", &base.into()).await?);
    }
    creation.pr = Some(pull.url.into());
    Ok(())
}

/// Settle `creation` on the head branch of `pull`, a PR from `repo`'s own
/// origin, without fetching: the local branch of the workspace that owns
/// it, which reopens, else the pushed branch; returns whether a workspace
/// owns it.
pub async fn pull_head_settled(
    reads: &impl LaunchReads,
    repo: &Repository,
    pull: &PullRequest,
    creation: &mut Creation,
) -> Result<bool> {
    let owned = reads.workspaces().await?.iter().any(|workspace| {
        workspace.repository() == &repo.id
            && workspace.branch().map(BranchName::as_str) == Some(pull.head.as_str())
    });
    creation.existing = Some(match owned {
        true => pull.head.as_str().to_owned(),
        false => git::remote_ref("origin", pull.head.as_str()),
    });
    Ok(owned)
}

/// Refuse what `creation` asks of a branch when it opens `pull`, a PR from
/// a fork, whose head opens at a commit in a workspace Shoal places.
pub fn ensure_fork_opens(pull: &PullRequest, creation: &Creation) -> Result<()> {
    ensure!(
        creation.path.is_none() && creation.git_profile.is_none() && creation.base.is_none(),
        "PR #{} comes from a fork and opens at its head in a workspace Shoal places; --path, --git-profile and --base apply to a branch",
        pull.number
    );
    Ok(())
}

/// The head of `pull`, a PR from a fork in `repo`, which only the ref the
/// forge serves it under reaches, with its base branch fetched for its
/// workspace's diff to compare with.
pub async fn fork_head(repo: &Repository, pull: &PullRequest) -> Result<PullHead> {
    let reference = pull.head_ref.clone().with_context(|| {
        format!(
            "PR #{} comes from a fork, and its forge serves no ref for a fork's head, so Shoal cannot open it",
            pull.number
        )
    })?;
    let commit = pull
        .head_commit
        .clone()
        .with_context(|| format!("PR #{} reports no head commit", pull.number))?;
    let base = fetch_pull_branch(&repo.path, "base", &pull.base).await?;
    Ok(PullHead {
        number: pull.number,
        reference,
        commit,
        base: Some(base),
    })
}

/// Fetch the PR's `role` branch, such as its base, into the checkout at
/// `path`, returning the ref it is fetched to.
pub async fn fetch_pull_branch(
    path: &std::path::Path,
    role: &str,
    branch: &BranchName,
) -> Result<String> {
    git::check_branch_name(branch.as_str())
        .with_context(|| format!("PR {role} {branch:?} is not a branch name"))?;
    git::fetch::review_branch(path, branch)
        .await
        .with_context(|| format!("could not fetch the PR {role} {branch}"))
}

/// Settle the branch `creation` names as typed in `repo`, which `selector`
/// names: the workspace that already has it, which the start goes into
/// instead, else the existing branch `creation` then opens, else the new
/// branch it creates, which a name already taken would suffix and is
/// refused.
pub async fn settle(
    reads: &impl LaunchReads,
    (selector, repo): (&crate::repository::Selector, &Repository),
    creation: &mut Creation,
) -> Result<Option<Workspace>> {
    let Some(name) = creation.branch.take() else {
        return Ok(None);
    };
    let (remote, mut taken) = branches(repo, &name).await?;
    let workspaces = repository_workspaces(reads, repo).await?;
    if let Some(workspace) = super::on_branch(&workspaces, &name, remote.as_ref()) {
        // A remote branch opens a local one only where it tracks it.
        if remote.is_some() {
            reads.check_opens(selector, &name).await?;
        }
        return Ok(Some(workspace.clone()));
    }
    if remote.is_some() || taken.contains(&name.as_str().into()) {
        ensure!(
            creation.copy.is_none(),
            "a copy starts a new branch, and branch {name} exists; name another"
        );
        creation.existing = Some(name);
        return Ok(None);
    }
    taken.extend(
        workspaces
            .into_iter()
            .filter_map(|workspace| workspace.branch().cloned()),
    );
    ensure!(
        git::repo::allocate_branch(&name, &taken) == name,
        "branch {name} conflicts with an existing branch or workspace; name another"
    );
    creation.branch = Some(name);
    creation.typed = true;
    Ok(None)
}

/// The workspace of `repo` that already has the branch `name` selects, as
/// typed or as a remote's, which a start on it goes into.
pub async fn owning_workspace(
    reads: &impl LaunchReads,
    repo: &Repository,
    name: &str,
) -> Result<Option<Workspace>> {
    let remote = git::remote_branch(&repo.path, name).await?;
    let workspaces = repository_workspaces(reads, repo).await?;
    Ok(super::on_branch(&workspaces, name, remote.as_ref()).cloned())
}

/// The workspaces of `repo`.
async fn repository_workspaces(
    reads: &impl LaunchReads,
    repo: &Repository,
) -> Result<Vec<Workspace>> {
    Ok((reads.workspaces().await?.into_iter())
        .filter(|workspace| workspace.repository() == &repo.id)
        .collect())
}

/// The remote and branch `name` names in `repo`, if it names a remote one,
/// and the branch names a new branch must not take.
async fn branches(
    repo: &Repository,
    name: &str,
) -> Result<(Option<(String, BranchName)>, Vec<BranchName>)> {
    let read = async {
        let remote = git::remote_branch(&repo.path, name).await?;
        anyhow::Ok((remote, git::branch::names_in_use(&repo.path).await?))
    };
    let error = match read.await {
        Ok(read) => return Ok(read),
        Err(error) => error,
    };
    let name = repo.display_name();
    // Removed meanwhile, as `shoal repo rm` takes its checkout.
    if !crate::fsutil::off_runtime::try_exists(&repo.path)
        .await
        .unwrap_or(true)
    {
        let message = format!("repository {name} ({}) was removed", repo.id);
        return Err(crate::refusal::Refusal::new(crate::refusal::Reason::NotFound, message).into());
    }
    // As while a removal deletes its files: named by its ID, which a
    // retry of either takes.
    Err(error.context(format!(
        "read the branches of repository {name} ({})",
        repo.id
    )))
}

/// `creation` on the branch the issue at `url` with `title` settles in
/// `repo` when nothing else names one: the branch or workspace earlier
/// work on it left, else a new branch named after it.
pub async fn issue_settled(
    reads: &impl LaunchReads,
    repo: &Repository,
    creation: Creation,
    issue: Option<(&ItemRef, &str)>,
) -> Result<Creation> {
    match issue {
        Some((url, title)) if creation.unsettled() => {
            let branch = issue_branch(url.issue()?.get(), title);
            issue_creation(reads, repo, creation, (branch, url)).await
        }
        _ => Ok(creation),
    }
}

/// `creation` on the branch `name` of an issue at `issue_url`: the local
/// branch or workspace earlier work on the issue left, which reopens rather
/// than suffixing a new branch and is refused here when it cannot, else
/// the new branch `name`.
pub async fn issue_creation(
    reads: &impl LaunchReads,
    repo: &Repository,
    mut creation: Creation,
    (name, issue_url): (String, &ItemRef),
) -> Result<Creation> {
    let workspaces = reads.workspaces().await?;
    let mut associated = None;
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.repository() == &repo.id)
    {
        if (workspace.links.issues.iter()).any(|issue| issue.url == *issue_url) {
            ensure!(
                associated.is_none(),
                "multiple workspaces are associated with this issue; pass an explicit branch"
            );
            ensure_reopens(workspace)?;
            associated = Some(
                workspace
                    .require_branch("reopening for an issue")?
                    .to_owned(),
            );
        }
    }
    if let Some(branch) = associated {
        creation.existing = Some(branch.into());
        return Ok(creation);
    }
    let local = BranchName::from(name.as_str());
    if let Some(workspace) = workspaces
        .into_iter()
        .find(|workspace| workspace.repository() == &repo.id && workspace.branch() == Some(&local))
    {
        ensure_reopens(&workspace)?;
    } else if git::ref_exists(&repo.path, &git::local_ref(&local)).await? {
        git::existing_branch::ensure_not_checked_out(&repo.path, &local).await?;
    } else {
        creation.branch = Some(name);
        return Ok(creation);
    }
    creation.existing = Some(name);
    Ok(creation)
}

/// The first flag of a start that only the new workspace of `creation` takes,
/// with an issue of its own when `issue` and below a parent it names when
/// `parent`, which a start into the workspace that has its branch refuses.
pub fn new_space_flag(creation: &Creation, issue: bool, parent: bool) -> Option<&'static str> {
    let new_space = [
        ("--issue", issue),
        ("--name", creation.options.name.is_some()),
        ("--parent", parent),
        ("--base", creation.base.is_some()),
        ("--path", creation.path.is_some()),
        ("--git-profile", creation.git_profile.is_some()),
        ("--copy", creation.copy.is_some()),
        ("--skip-setup", creation.options.skip_setup),
        ("--disposable", creation.options.disposable),
        // `shoal priority` sets an existing workspace's.
        ("--priority", creation.options.priority.is_some()),
    ];
    (new_space.into_iter())
        .find(|(_, set)| *set)
        .map(|(flag, _)| flag)
}

/// The name of the workspace `creation` opens in `repo`: the one it gives,
/// else the one it derives. An existing branch opens under its local name,
/// so a remote's prefix is left out, as opening it does.
pub async fn planned_name(repo: &Repository, creation: &Creation) -> Result<SpaceName> {
    if let Some(name) = &creation.options.name {
        return Ok(name.clone());
    }
    let branch = match (&creation.branch, opened_branch(repo, creation).await?) {
        (Some(branch), _) => branch.clone(),
        (None, Some(local)) => local,
        (None, None) => (creation.copy.as_ref()).map_or_else(String::new, |copy| copy.name.clone()),
    };
    Ok(SpaceName::derive(&branch, "workspace"))
}

/// The local name of the existing branch `creation` opens in `repo`, if
/// any.
async fn opened_branch(repo: &Repository, creation: &Creation) -> Result<Option<String>> {
    let Some(selector) = &creation.existing else {
        return Ok(None);
    };
    if let Some(local) = git::strip_local(selector) {
        return Ok(Some(local.to_owned()));
    }
    let remote = git::remote_branch(&repo.path, selector).await?;
    Ok(Some(remote.map_or_else(
        || selector.clone(),
        |(_, branch)| branch.into(),
    )))
}

/// The existing workspace of `repo` on the branch `creation` opens, which a
/// launch there reopens as that workspace's agent.
pub async fn reopened_workspace(
    reads: &impl LaunchReads,
    repo: &Repository,
    creation: &Creation,
) -> Result<Option<SpaceId>> {
    let Some(branch) = opened_branch(repo, creation).await? else {
        return Ok(None);
    };
    Ok((reads.workspaces().await?.into_iter())
        .find(|workspace| {
            workspace.repository() == &repo.id
                && workspace.branch().map(BranchName::as_str) == Some(&branch)
        })
        .map(|workspace| workspace.id))
}

/// Refuse to reopen `workspace` unless it is ready.
fn ensure_reopens(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == WorkspaceState::Ready,
        "workspace {} is {}; inspect or set it up before reopening",
        workspace.name,
        workspace.state
    );
    Ok(())
}

#[cfg(test)]
mod tests;
