//! The processes of each space, a workspace or chat, that Shoal does not
//! track: those carrying its identity, and its leftovers, detached processes
//! tied to it with every process they started. A process a tracked one
//! started, or one that started a tracked one, belongs to that one instead.
use super::{
    Identity, Inventory, Lineage, MAX_ANCESTRY, Marks, descends_from_detached, executable,
    inventory, owners, working_directory,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// What ties processes to a space.
#[derive(Debug, Clone)]
pub struct Space {
    pub id: String,
    /// The workspace's worktree, where its processes run and its programs
    /// are built; a chat has none.
    pub worktree: Option<PathBuf>,
    /// Its directory for temporary files.
    pub tmp_dir: Option<PathBuf>,
}

impl Space {
    /// The space with its directories as processes report them, through
    /// symlinks such as macOS's `/tmp`.
    fn canonical(self) -> Self {
        let canonical = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
        Self {
            worktree: self.worktree.map(canonical),
            tmp_dir: self.tmp_dir.map(canonical),
            ..self
        }
    }

    /// Whether a process working in `directory` and running `program` is
    /// tied to the space by where it runs.
    fn places(&self, directory: Option<&Path>, program: Option<&Path>) -> bool {
        let inside = |path: Option<&Path>, root: Option<&PathBuf>| {
            path.zip(root)
                .is_some_and(|(path, root)| path.starts_with(root))
        };
        inside(directory, self.worktree.as_ref())
            || inside(directory, self.tmp_dir.as_ref())
            || inside(program, self.worktree.as_ref())
    }
}

/// A process of a space that Shoal does not track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Untracked {
    pub identity: Identity,
    /// Its working directory when it was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// It left what started it behind, so only a stop ends it.
    pub leftover: bool,
}

/// Programs serving terminal sessions, whose servers launchd adopts by
/// design: the sessions they serve, such as a shell Shoal opened for a
/// workspace, are the user's, not leftovers.
const TERMINAL_SERVERS: [&str; 4] = ["zmx", "tmux", "screen", "herdr"];

/// The untracked processes of each of `spaces`, by space ID: none of them is
/// one of `tracked`, such as an execution's wrapper or a chat's agent, a
/// process one of them started, or one that started one of them, nor a hook
/// that a tracked process the inventory does not list, such as the daemon
/// running the survey, still runs.
pub async fn survey(
    spaces: Vec<Space>,
    tracked: Vec<Identity>,
) -> Result<HashMap<String, Vec<Untracked>>> {
    let spaces: Vec<Space> = spaces.into_iter().map(Space::canonical).collect();
    inventory(&owners::OWNERS, move |inventory| {
        let survey = Survey::of(inventory, &tracked);
        Ok(spaces
            .iter()
            .map(|space| (space.id.clone(), survey.untracked(space)))
            .collect())
    })
    .await
}

/// What one inventory tells about the processes this user runs.
struct Survey<'a> {
    lineages: HashMap<u32, &'a Lineage>,
    marks: HashMap<u32, Marks>,
    directories: HashMap<u32, PathBuf>,
    programs: HashMap<u32, PathBuf>,
    /// Processes of tracked ones, and terminal servers.
    excluded: HashSet<u32>,
}

impl<'a> Survey<'a> {
    fn of(inventory: &'a Inventory<'a>, tracked: &[Identity]) -> Self {
        let marks = inventory
            .attribute_all()
            .into_iter()
            .filter_map(|(pid, evidence)| match evidence.ok()?.told {
                owners::Told::Owner(marks) => Some((pid, marks)),
                owners::Told::NoExecution => None,
            })
            .collect();
        let lineages: HashMap<u32, &Lineage> = inventory
            .observed
            .iter()
            .filter(|(pid, _)| runs_as_this_user(inventory, **pid))
            .map(|(pid, observed)| (*pid, &observed.lineage))
            .collect();
        let pids = lineages.keys().copied();
        let mut survey = Self {
            directories: pids
                .clone()
                .filter_map(|pid| Some((pid, working_directory(pid)?)))
                .collect(),
            programs: pids
                .filter_map(|pid| Some((pid, executable(pid)?)))
                .collect(),
            lineages,
            marks,
            excluded: HashSet::new(),
        };
        survey.excluded = survey.excluded(tracked);
        survey
    }

    /// The processes of `tracked`, those they started, those that started
    /// them, and terminal servers.
    fn excluded(&self, tracked: &[Identity]) -> HashSet<u32> {
        let (listed, unlisted): (Vec<&Identity>, Vec<&Identity>) =
            tracked.iter().partition(|identity| {
                self.lineages
                    .get(&identity.pid)
                    .is_some_and(|lineage| lineage.identity == **identity)
            });
        let listed: HashSet<u32> = listed.iter().map(|identity| identity.pid).collect();
        // The inventory never lists the process running it, such as the
        // daemon, whose running hooks carry a workspace's identity.
        let unlisted: HashSet<u32> = unlisted
            .iter()
            .filter(|identity| super::alive(identity).unwrap_or(false))
            .map(|identity| identity.pid)
            .collect();
        let mut excluded: HashSet<u32> =
            listed.iter().flat_map(|pid| self.ancestry(*pid)).collect();
        for pid in self.lineages.keys() {
            let ancestry = self.ancestry(*pid);
            let unlisted_hook = self.marks.get(pid).is_some_and(|marks| marks.hook)
                && ancestry
                    .last()
                    .is_some_and(|oldest| unlisted.contains(&self.lineages[oldest].parent_pid));
            if ancestry.iter().any(|pid| listed.contains(pid))
                || unlisted_hook
                || self.terminal_server(*pid)
            {
                excluded.insert(*pid);
            }
        }
        excluded
    }

    fn terminal_server(&self, pid: u32) -> bool {
        self.programs
            .get(&pid)
            .and_then(|program| program.file_name()?.to_str())
            .is_some_and(|name| TERMINAL_SERVERS.contains(&name))
    }

    /// `pid` and its ancestors, nearest first, while each parent is the
    /// process its child was forked from.
    fn ancestry(&self, pid: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut current = self.lineages.get(&pid).copied();
        while let Some(lineage) = current
            && chain.len() < MAX_ANCESTRY
        {
            chain.push(lineage.identity.pid);
            current = self
                .lineages
                .get(&lineage.parent_pid)
                .copied()
                .filter(|parent| lineage.forked_from(parent));
        }
        chain
    }

    /// The space's processes carrying its identity, and its leftovers.
    fn untracked(&self, space: &Space) -> Vec<Untracked> {
        let candidates = || {
            self.lineages
                .keys()
                .copied()
                .filter(|pid| !self.excluded.contains(pid))
        };
        let marked: HashSet<u32> = candidates()
            .filter(|pid| {
                self.marks.get(pid).is_some_and(|marks| {
                    [&marks.workspace, &marks.chat]
                        .into_iter()
                        .any(|id| id.as_deref() == Some(space.id.as_str()))
                })
            })
            .collect();
        let tied: HashSet<u32> = candidates()
            .filter(|pid| {
                marked.contains(pid)
                    || space.places(
                        self.directories.get(pid).map(PathBuf::as_path),
                        self.programs.get(pid).map(PathBuf::as_path),
                    )
            })
            .collect();
        let detached: HashSet<u32> = tied
            .iter()
            .copied()
            .filter(|pid| {
                descends_from_detached(self.lineages[pid], |parent| {
                    tied.contains(&parent).then(|| self.lineages[&parent])
                })
            })
            .collect();
        candidates()
            .filter_map(|pid| {
                let leftover = self
                    .ancestry(pid)
                    .iter()
                    .any(|ancestor| detached.contains(ancestor));
                (leftover || marked.contains(&pid)).then(|| Untracked {
                    identity: self.lineages[&pid].identity.clone(),
                    directory: self.directories.get(&pid).cloned(),
                    leftover,
                })
            })
            .collect()
    }
}

/// Setuid programs this user started run as another user: Shoal neither
/// reports nor stops them.
fn runs_as_this_user(inventory: &Inventory, pid: u32) -> bool {
    !matches!(inventory.observed[&pid].evidence, super::Evidence::Elevated)
}

#[cfg(test)]
mod tests;
