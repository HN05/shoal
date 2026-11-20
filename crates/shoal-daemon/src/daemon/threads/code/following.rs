//! Following references as their code changes. A read that finds a
//! reference's file differs from the version it was last followed to maps
//! its lines through a diff of that version against the file now, and
//! records where they went, so each step stays small and exact. A file
//! renamed whole is found where it went; code gone from its file leaves
//! the reference outdated.
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    code::{self, CodeAt, CodeRef, CodeState, Lines},
    threads::Thread,
};

use super::{kept_text, records};
use crate::{daemon::manager::Manager, git, model::Workspace, state::WorkspaceState};

/// Where a reference went, and what became of its code.
struct Step {
    latest: CodeAt,
    state: CodeState,
}

impl Manager {
    /// `threads` with the references in each ready worktree followed to
    /// its code now; one that cannot be followed stays where it was.
    pub(in crate::daemon::threads) async fn follow_code(&self, threads: &mut [Thread]) {
        let mut by_space: BTreeMap<SpaceId, Vec<&mut CodeRef>> = BTreeMap::new();
        let references = (threads.iter_mut())
            .flat_map(|thread| thread.posts.iter_mut())
            .flat_map(|post| post.code.iter_mut());
        for code in references {
            by_space
                .entry(code.space.id.clone())
                .or_default()
                .push(code);
        }
        for (space, references) in by_space {
            if let Err(error) = self.follow_in(&space, references).await {
                crate::daemon::log!("code in {space} not followed: {error:#}");
            }
        }
    }

    /// Follow `references` into the worktree of the space `space`, while it
    /// is a ready workspace, keeping the versions they reach.
    async fn follow_in(&self, space: &SpaceId, mut references: Vec<&mut CodeRef>) -> Result<()> {
        let Some(workspace) = self.find_workspace(space).await? else {
            return Ok(());
        };
        if workspace.state != WorkspaceState::Ready {
            return Ok(());
        }
        let current = current_versions(&workspace.path, &references).await?;
        let mut moved = false;
        for code in &mut references {
            let now = current.get(&code.latest.path).map(String::as_str);
            if let Some(step) = step(&workspace.path, code, now).await? {
                moved |= self.record_step(code, step).await?;
            }
        }
        if moved {
            self.keep_versions_of_workspace(&workspace).await;
        }
        Ok(())
    }

    /// Move `code` to where `step` took it, unless another read moved it
    /// meanwhile; whether it moved.
    async fn record_step(&self, code: &mut CodeRef, step: Step) -> Result<bool> {
        let (id, from) = (code.id, code.latest.clone());
        let (latest, state) = (step.latest.clone(), step.state);
        let recorded = (self.store)
            .write(move |tx| records::follow(tx, id, &from, &latest, state))
            .await?;
        if recorded {
            (code.latest, code.state) = (step.latest, step.state);
        }
        Ok(recorded)
    }

    /// Keep the versions the references in `workspace`'s worktree reach.
    async fn keep_versions_of_workspace(&self, workspace: &Workspace) {
        if let Some(repository) = &workspace.repository_id {
            (self.keep_code_versions(repository, [workspace.id.clone()])).await;
        }
    }
}

/// The version of each file of `references` in the worktree `root` that
/// is still there, by path, read in one Git call.
async fn current_versions(
    root: &Path,
    references: &[&mut CodeRef],
) -> Result<HashMap<String, String>> {
    let mut paths: Vec<&str> = (references.iter())
        .filter(|code| code.latest.blob.is_some() && root.join(&code.latest.path).is_file())
        .map(|code| code.latest.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let blobs = git::code_versions::hash(root, &paths).await?;
    Ok(paths.into_iter().map(str::to_owned).zip(blobs).collect())
}

/// Where `code` went in the worktree `root`, whose file at its path is
/// version `now` while it is there; none while nothing changed.
async fn step(root: &Path, code: &CodeRef, now: Option<&str>) -> Result<Option<Step>> {
    let Some(blob) = &code.latest.blob else {
        let gone = !root.join(&code.latest.path).exists();
        return Ok(gone.then(|| outdated(code)).flatten());
    };
    if now == Some(blob.as_str()) {
        return Ok(None);
    }
    let path = match now {
        Some(_) => code.latest.path.clone(),
        None => match renamed(root, code, blob).await? {
            Some(path) => path,
            None => return Ok(outdated(code)),
        },
    };
    let file = root.join(&path);
    let read = crate::fsutil::off_runtime::run(move || Ok(std::fs::read(file).ok()));
    let Some(text) = read.await?.and_then(kept_text) else {
        return Ok(outdated(code));
    };
    let before = git::code_versions::read(root, blob).await?;
    let (lines, state) = mapped(code.latest.lines, &before, &text);
    let blob = git::code_versions::write(root, text).await?;
    Ok(Some(Step {
        latest: CodeAt {
            path,
            lines,
            blob: Some(blob),
        },
        state: code.state.then(state),
    }))
}

/// Where `lines` of `before` are in `after`, and what became of them; a
/// whole file changed.
fn mapped(lines: Option<Lines>, before: &str, after: &str) -> (Option<Lines>, CodeState) {
    match lines {
        Some(lines) => {
            let followed = code::follow(before, after, lines);
            (Some(followed.lines), followed.state)
        }
        None => (None, CodeState::Changed),
    }
}

/// `code` where it was, outdated, unless it is already.
fn outdated(code: &CodeRef) -> Option<Step> {
    (code.state != CodeState::Outdated).then(|| Step {
        latest: code.latest.clone(),
        state: CodeState::Outdated,
    })
}

/// Where the file of `code`, last at version `blob`, went if it was
/// renamed whole since the commit the reference was made at.
async fn renamed(root: &Path, code: &CodeRef, blob: &str) -> Result<Option<String>> {
    let Some(commit) = &code.commit else {
        return Ok(None);
    };
    git::code_versions::renamed(root, commit, &code.latest.path, blob).await
}
