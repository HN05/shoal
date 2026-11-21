//! Following references, and the lines of changes, as their code changes.
//! A read that finds a file differs from the version a reference or change
//! was last followed to maps its lines through a diff of that version
//! against the file now, and records where they went, so each step stays
//! small and exact. A file renamed whole is found where it went; code gone
//! from its file leaves it outdated.
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    changes::Change,
    code::{self, CodeAt, CodeRef, CodeState, Lines},
    threads::Thread,
};
use shoal_refs::CommitId;

use super::{kept_text, records};
use crate::{daemon::manager::Manager, git, model::Workspace, state::WorkspaceState};

/// Where a reference or change went, and what became of its code.
struct Step {
    latest: CodeAt,
    state: CodeState,
}

/// What follows code: a reference a post makes, or a change's lines.
pub(in crate::daemon) trait Followed: Send + Sync {
    /// The space whose worktree it is in.
    fn space(&self) -> &SpaceId;
    /// HEAD when it was made, from which a rename is found.
    fn commit(&self) -> Option<&CommitId>;
    fn latest(&self) -> &CodeAt;
    fn state(&self) -> CodeState;
    /// Its record, which a step moves.
    fn row(&self) -> Row;
    fn moved(&mut self, latest: CodeAt, state: CodeState);
}

/// The record of something that follows code.
#[derive(Debug, Clone)]
pub(in crate::daemon) enum Row {
    Reference(i64),
    Change { space: SpaceId, number: u32 },
}

impl Followed for CodeRef {
    fn space(&self) -> &SpaceId {
        &self.space.id
    }

    fn commit(&self) -> Option<&CommitId> {
        self.commit.as_ref()
    }

    fn latest(&self) -> &CodeAt {
        &self.latest
    }

    fn state(&self) -> CodeState {
        self.state
    }

    fn row(&self) -> Row {
        Row::Reference(self.id)
    }

    fn moved(&mut self, latest: CodeAt, state: CodeState) {
        (self.latest, self.state) = (latest, state);
    }
}

impl Followed for Change {
    fn space(&self) -> &SpaceId {
        &self.space.id
    }

    fn commit(&self) -> Option<&CommitId> {
        self.commit.as_ref()
    }

    fn latest(&self) -> &CodeAt {
        &self.latest
    }

    fn state(&self) -> CodeState {
        self.state
    }

    fn row(&self) -> Row {
        Row::Change {
            space: self.space.id.clone(),
            number: self.number,
        }
    }

    fn moved(&mut self, latest: CodeAt, state: CodeState) {
        (self.latest, self.state) = (latest, state);
    }
}

impl Manager {
    /// `threads` with the references and changes their posts point at in
    /// each ready worktree followed to their code now.
    pub(in crate::daemon::threads) async fn follow_code(&self, threads: &mut [Thread]) {
        let posts = threads
            .iter_mut()
            .flat_map(|thread| thread.posts.iter_mut());
        let mut followed: Vec<&mut dyn Followed> = Vec::new();
        for post in posts {
            followed.extend(post.code.iter_mut().map(|code| code as &mut dyn Followed));
            let changes = post.changes.iter_mut().map(|diff| &mut diff.change);
            followed.extend(changes.map(|change| change as &mut dyn Followed));
        }
        self.follow(followed).await;
    }

    /// `followed` in each ready worktree followed to its code now; one that
    /// cannot be followed stays where it was, and the others go on.
    pub(in crate::daemon) async fn follow(&self, followed: Vec<&mut dyn Followed>) {
        let mut by_space: BTreeMap<SpaceId, Vec<&mut dyn Followed>> = BTreeMap::new();
        for code in followed {
            by_space.entry(code.space().clone()).or_default().push(code);
        }
        for (space, references) in by_space {
            if let Err(error) = self.follow_in(&space, references).await {
                crate::daemon::log!("code in {space} not followed: {error:#}");
            }
        }
    }

    /// Follow `references` into the worktree of the space `space`, while it
    /// is a ready workspace, keeping the versions they reach.
    async fn follow_in(
        &self,
        space: &SpaceId,
        mut references: Vec<&mut dyn Followed>,
    ) -> Result<()> {
        let Some(workspace) = self.find_workspace(space).await? else {
            return Ok(());
        };
        if workspace.state != WorkspaceState::Ready {
            return Ok(());
        }
        let current = current_versions(&workspace.path, &references).await?;
        let mut moved = false;
        for code in &mut references {
            let now = current.get(&code.latest().path).map(String::as_str);
            match self.follow_one(&workspace.path, &mut **code, now).await {
                Ok(followed) => moved |= followed,
                Err(error) => {
                    crate::daemon::log!("{:?} not followed: {error:#}", code.row())
                }
            }
        }
        if moved {
            self.keep_versions_of_workspace(&workspace).await;
        }
        Ok(())
    }

    /// Follow `code` into the worktree `root`, whose file at its path is
    /// version `now` while it is there; whether it moved.
    async fn follow_one(
        &self,
        root: &Path,
        code: &mut dyn Followed,
        now: Option<&str>,
    ) -> Result<bool> {
        match step(root, code, now).await? {
            Some(step) => self.record_step(code, step).await,
            None => Ok(false),
        }
    }

    /// Move `code` to where `step` took it, unless another read moved it
    /// meanwhile; whether it moved.
    async fn record_step(&self, code: &mut dyn Followed, step: Step) -> Result<bool> {
        let (row, from) = (code.row(), code.latest().clone());
        let (latest, state) = (step.latest.clone(), step.state);
        let recorded = (self.store)
            .write(move |tx| match &row {
                Row::Reference(id) => records::follow(tx, *id, &from, &latest, state),
                Row::Change { space, number } => {
                    let follow = crate::daemon::changes::records::follow;
                    follow(tx, space, *number, &from, &latest, state)
                }
            })
            .await?;
        if recorded {
            code.moved(step.latest, step.state);
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
    references: &[&mut dyn Followed],
) -> Result<HashMap<String, String>> {
    let mut paths: Vec<&str> = (references.iter())
        .map(|code| code.latest())
        .filter(|latest| latest.blob.is_some() && root.join(&latest.path).is_file())
        .map(|latest| latest.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let blobs = git::code_versions::hash(root, &paths).await?;
    Ok(paths.into_iter().map(str::to_owned).zip(blobs).collect())
}

/// Where `code` went in the worktree `root`, whose file at its path is
/// version `now` while it is there; none while nothing changed.
async fn step(root: &Path, code: &dyn Followed, now: Option<&str>) -> Result<Option<Step>> {
    let Some(blob) = &code.latest().blob else {
        let gone = !root.join(&code.latest().path).exists();
        return Ok(gone.then(|| outdated(code)).flatten());
    };
    if now == Some(blob.as_str()) {
        return Ok(None);
    }
    // Code once gone stays outdated until its file is back at its path, so
    // no read searches for a rename again.
    if now.is_none() && code.state() == CodeState::Outdated {
        return Ok(None);
    }
    let path = match now {
        Some(_) => code.latest().path.clone(),
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
    let (lines, state) = mapped(code.latest().lines, &before, &text);
    let blob = git::code_versions::write(root, text).await?;
    Ok(Some(Step {
        latest: CodeAt {
            path,
            lines,
            blob: Some(blob),
        },
        state: code.state().then(state),
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
fn outdated(code: &dyn Followed) -> Option<Step> {
    (code.state() != CodeState::Outdated).then(|| Step {
        latest: code.latest().clone(),
        state: CodeState::Outdated,
    })
}

/// Where the file of `code`, last at version `blob`, went if it was
/// renamed whole since the commit the reference was made at.
async fn renamed(root: &Path, code: &dyn Followed, blob: &str) -> Result<Option<String>> {
    let Some(commit) = code.commit() else {
        return Ok(None);
    };
    git::code_versions::renamed(root, commit, &code.latest().path, blob).await
}
