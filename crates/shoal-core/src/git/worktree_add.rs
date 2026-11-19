//! Running `git worktree add` past a worktree prune. Git's maintenance,
//! which commands such as `commit` and `fetch` start in the background in
//! any of a repository's worktrees, prunes every admin directory that does
//! not yet name its worktree: one a concurrent add has just made too,
//! before it locks it. No gate of Shoal's keeps another Git process out, so
//! an add that loses that race runs again.
use anyhow::Result;
use tokio::process::Command;

/// How many times an add runs before a prune's failure stands.
const ATTEMPTS: usize = 3;

/// Run the `git worktree add` that `command` builds, again while a prune
/// deleted the admin directory it made, up to [`ATTEMPTS`] times; its
/// stdout. Each run must do the same, so it creates no branch: such a
/// failure leaves the branch it created.
pub(crate) async fn run(command: impl Fn() -> Command) -> Result<String> {
    let mut attempt = 1;
    loop {
        match super::output(command()).await {
            Err(error) if attempt < ATTEMPTS && pruned(&error) => attempt += 1,
            result => return result,
        }
    }
}

/// Whether `error` is an add's failure to lock the admin directory it made,
/// which only a deletion meanwhile causes: the one failure that names a
/// `locked` file among the repository's worktrees, in whatever language Git
/// reports it.
fn pruned(error: &anyhow::Error) -> bool {
    format!("{error:#}")
        .lines()
        .any(|line| line.contains("worktrees/") && line.contains("/locked"))
}

#[cfg(test)]
mod tests;
