//! The turn-state reports agents' hooks send the daemon, each kept on disk
//! while it is sent: a hook whose time limit ends it, or whose daemon
//! refuses it or cannot be reached, leaves its report behind for the daemon
//! to find, so the loss shows and the state the report carried still
//! arrives.
use std::{
    fs::{self, File},
    io::{self, Read as _},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    client::{agent::AgentSessionId, execution_ids::ExecutionId, space_ids::SpaceId},
    fsutil::atomic::{Permissions, ReplaceOptions},
    paths::Paths,
    protocol::TurnEnd,
    state::{AgentState, TurnFailure},
};

/// What a hook reports for its space's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    /// The agent's turn state, ending its turn as `turn_end` says when it
    /// has one.
    State {
        state: AgentState,
        session: Option<AgentSessionId>,
        turn_end: Option<TurnEnd>,
    },
    /// The turn `failure` ended.
    TurnFailure {
        failure: TurnFailure,
        session: Option<AgentSessionId>,
        turn_end: TurnEnd,
    },
}

/// A report as its file keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub report: Report,
    /// The execution the agent runs in, as the hook's environment names it.
    pub execution: Option<ExecutionId>,
    /// The processes the hook ran under, nearest first, for an agent that
    /// runs in no execution, such as a chat's: one of them is the agent
    /// that made the report, which a restore replaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestors: Vec<u32>,
    /// When the hook made the report.
    pub at: SystemTime,
    /// Why the daemon did not record it, when the hook learned why before
    /// it ended.
    #[serde(default)]
    pub error: Option<String>,
}

/// A report this process sends, whose file it holds locked so that the
/// daemon leaves it alone until the process ends.
pub struct Sending {
    _lock: File,
    path: PathBuf,
    kept: Kept,
}

impl Sending {
    /// Keep `report` of the agent of `space`, which runs in `execution`.
    /// The daemon only finds the file whole and locked: it is written and
    /// locked under a hidden name, then renamed.
    pub fn keep(
        paths: &Paths,
        space: &SpaceId,
        report: Report,
        execution: Option<ExecutionId>,
    ) -> io::Result<Self> {
        let directory = paths.hook_reports().join(space);
        fs::create_dir_all(&directory)?;
        let ancestors = match execution {
            Some(_) => Vec::new(),
            None => crate::process::identity::ancestors().collect(),
        };
        let kept = Kept {
            report,
            execution,
            ancestors,
            at: SystemTime::now(),
            error: None,
        };
        let path = directory.join(file_name(kept.at));
        let file = locked_copy(&directory, &kept)?
            .persist_noclobber(&path)
            .map_err(|error| error.error)?;
        Ok(Self {
            _lock: file,
            path,
            kept,
        })
    }

    /// When the hook made the report.
    pub fn made(&self) -> SystemTime {
        self.kept.at
    }

    /// The daemon received the report, whose file becomes the space's mark
    /// of the latest report delivered, unless a later one already is. The
    /// mark's lock keeps hooks delivering at once from replacing a later
    /// mark with an earlier one.
    pub fn delivered(self) {
        let mark = self.path.with_file_name(DELIVERED);
        let Ok(_lock) = lock_mark(&self.path.with_file_name(DELIVERED_LOCK)) else {
            let _ = fs::remove_file(&self.path);
            return;
        };
        let _ = match delivered_at(&mark) {
            Some(at) if at > self.kept.at => fs::remove_file(&self.path),
            _ => fs::rename(&self.path, &mark),
        };
    }

    /// The daemon did not record the report, for `error`, which its file
    /// keeps for the daemon to tell. The file is replaced whole, by a copy
    /// locked before it, so a hook killed meanwhile leaves one of the two.
    pub fn failed(mut self, error: String) {
        self.kept.error = Some(error);
        let directory = self.path.parent().unwrap_or(Path::new("."));
        if let Ok(copy) = locked_copy(directory, &self.kept)
            && let Ok(file) = copy.persist(&self.path)
        {
            self._lock = file;
        }
    }
}

/// Names starting so are files a hook still writes, and the mark of the
/// latest report delivered.
const HIDDEN: &str = ".";

/// The file naming when the latest report the daemon received was made.
const DELIVERED: &str = ".delivered";

/// The lock held while the mark of the latest report delivered changes.
const DELIVERED_LOCK: &str = ".delivered.lock";

/// The lock at `path` on the mark of the latest report delivered, held.
fn lock_mark(path: &Path) -> io::Result<File> {
    let lock = File::create(path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

/// When the report whose file is `mark` was made.
fn delivered_at(mark: &Path) -> Option<SystemTime> {
    let kept: Kept = serde_json::from_slice(&fs::read(mark).ok()?).ok()?;
    Some(kept.at)
}

/// When the latest report from a hook of `space` that the daemon received
/// was made, as far as the hooks' marks tell, which outlast the daemon.
pub fn last_delivered(paths: &Paths, space: &SpaceId) -> Option<SystemTime> {
    delivered_at(&paths.hook_reports().join(space).join(DELIVERED))
}

/// Mark `kept`, a report of `space` the daemon recorded after its hook
/// ended, as the latest delivered, unless a later one already is.
pub fn mark_delivered(paths: &Paths, space: &SpaceId, kept: &Kept) -> io::Result<()> {
    let directory = paths.hook_reports().join(space);
    let _lock = lock_mark(&directory.join(DELIVERED_LOCK))?;
    let mark = directory.join(DELIVERED);
    if delivered_at(&mark).is_some_and(|at| at > kept.at) {
        return Ok(());
    }
    let options = ReplaceOptions {
        permissions: Permissions::Temporary,
        sync: false,
    };
    crate::fsutil::atomic::replace_atomically(&mark, &serde_json::to_vec(kept)?, options)
}

/// A report's file name, unique to the process that made it at `at`.
fn file_name(at: SystemTime) -> String {
    let nanos = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{nanos}-{}.json", std::process::id())
}

/// `kept` written to a hidden file in `directory`, locked before it is
/// written, for a rename to put in place whole.
fn locked_copy(directory: &Path, kept: &Kept) -> io::Result<tempfile::NamedTempFile> {
    let mut copy = tempfile::Builder::new()
        .prefix(HIDDEN)
        .tempfile_in(directory)?;
    fs2::FileExt::lock_exclusive(copy.as_file())?;
    serde_json::to_writer(copy.as_file_mut(), kept)?;
    Ok(copy)
}

/// The spaces whose hooks left reports, or have one in flight.
pub fn spaces(paths: &Paths) -> io::Result<Vec<SpaceId>> {
    let entries = match fs::read_dir(paths.hook_reports()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut spaces = Vec::new();
    for entry in entries {
        if let Some(name) = entry?.file_name().to_str() {
            spaces.push(SpaceId::from(name.to_owned()));
        }
    }
    Ok(spaces)
}

/// A report a hook left, held locked until the daemon has handled it; one
/// dropped before stays for a later look.
pub struct Left {
    _lock: File,
    path: PathBuf,
    /// The report, or `None` when its file does not read as one.
    pub kept: Option<Kept>,
}

impl Left {
    /// The daemon handled the report, which needs its file no more.
    pub fn handled(self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

/// The reports of a space's hooks the daemon finds.
pub struct Found {
    /// Those whose hook ended without the daemon receiving them.
    pub left: Vec<Left>,
    /// When the latest report a hook still sends was made, which may come
    /// after a left one.
    pub sending: Option<SystemTime>,
}

/// The reports the hooks of `space` left, and when the latest of those
/// they still send was made, which a file shows whole while its hook holds
/// it.
pub fn find(paths: &Paths, space: &SpaceId) -> io::Result<Found> {
    let mut found = Found {
        left: Vec::new(),
        sending: None,
    };
    let entries = match fs::read_dir(paths.hook_reports().join(space)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(HIDDEN) {
            continue;
        }
        let path = entry.path();
        match lock_left(&path)? {
            Held::Left(mut lock) => {
                let mut contents = Vec::new();
                lock.read_to_end(&mut contents)?;
                let kept = serde_json::from_slice(&contents).ok();
                found.left.push(Left {
                    _lock: lock,
                    path,
                    kept,
                });
            }
            Held::Sending(mut file) => {
                let mut contents = Vec::new();
                file.read_to_end(&mut contents)?;
                let made = serde_json::from_slice::<Kept>(&contents)
                    .ok()
                    .map(|kept| kept.at);
                found.sending = found.sending.max(made);
            }
            Held::Gone => {}
        }
    }
    Ok(found)
}

/// When the latest report a hook of `space` still sends was made.
pub fn sending(paths: &Paths, space: &SpaceId) -> io::Result<Option<SystemTime>> {
    Ok(find(paths, space)?.sending)
}

/// Forget the reports of `space`, which is gone.
pub fn forget(paths: &Paths, space: &SpaceId) -> io::Result<()> {
    match fs::remove_dir_all(paths.hook_reports().join(space)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Who holds the report file at `path`.
enum Held {
    /// The daemon now, its hook having ended.
    Left(File),
    /// Its hook, which still sends it.
    Sending(File),
    /// Nobody: it went meanwhile, as a delivered report's file does before
    /// its hook lets go of the lock.
    Gone,
}

/// The report at `path`, locked unless its hook still holds it.
fn lock_left(path: &Path) -> io::Result<Held> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Held::Gone),
        Err(error) => return Err(error),
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => {
            return Ok(Held::Sending(file));
        }
        Err(error) => return Err(error),
    }
    let held = file.metadata()?;
    let current = match fs::metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Held::Gone),
        Err(error) => return Err(error),
    };
    Ok(
        match (held.dev(), held.ino()) == (current.dev(), current.ino()) {
            true => Held::Left(file),
            false => Held::Gone,
        },
    )
}

#[cfg(test)]
mod tests;
