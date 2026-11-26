//! What a launch that creates or reopens a workspace starts on: a PR's head,
//! the workspace that owns a branch, and the branch an issue names.
use anyhow::{Context as _, Result, ensure};
use shoal_refs::BranchName;

use crate::{
    forge::{ForgeRepo, PullRequest},
    git,
    model::{Repository, Workspace},
    protocol::PullHead,
    start_plan::Creation,
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

/// Reopen the workspace that owns the PR's head branch, or open the pushed
/// branch compared against the PR's base; either way the PR is linked.
/// `workspaces` are those the caller sees.
pub async fn pull_creation(
    workspaces: &[Workspace],
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
    let owned = pull_head_settled(workspaces, repo, &pull, creation);
    if !owned && creation.base.is_none() {
        let base = pull.base.as_str();
        creation.base = Some(fetch_pull_branch(&repo.path, "base", &base.into()).await?);
    }
    creation.pr = Some(pull.url.into());
    Ok(())
}

/// Settle `creation` on the head branch of `pull`, a PR from `repo`'s own
/// origin, without fetching: the local branch of the one of `workspaces`
/// that owns it, which reopens, else the pushed branch; returns whether a
/// workspace owns it.
pub fn pull_head_settled(
    workspaces: &[Workspace],
    repo: &Repository,
    pull: &PullRequest,
    creation: &mut Creation,
) -> bool {
    let owned = workspaces.iter().any(|workspace| {
        workspace.repository() == &repo.id
            && workspace.branch().map(BranchName::as_str) == Some(pull.head.as_str())
    });
    creation.existing = Some(match owned {
        true => pull.head.as_str().to_owned(),
        false => git::remote_ref("origin", pull.head.as_str()),
    });
    owned
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

/// The one of `workspaces` in `repo` that already has the branch `name`
/// selects, as typed or as a remote's, which a start on it goes into.
pub async fn owning_workspace(
    workspaces: &[Workspace],
    repo: &Repository,
    name: &str,
) -> Result<Option<Workspace>> {
    let remote = git::remote_branch(&repo.path, name).await?;
    let workspaces = repository_workspaces(workspaces, repo);
    Ok(super::on_branch(&workspaces, name, remote.as_ref()).cloned())
}

pub fn repository_workspaces(workspaces: &[Workspace], repo: &Repository) -> Vec<Workspace> {
    (workspaces.iter())
        .filter(|workspace| workspace.repository() == &repo.id)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
