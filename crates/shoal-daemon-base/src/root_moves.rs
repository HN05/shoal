//! What a move of `root_dir` makes of the chats without a repository,
//! whose files move with it, and of the commands and agents running there.
use std::path::PathBuf;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::Execution;
use shoal_protocol::space_names::SpaceName;

/// What checking a repository makes of the commands and agents running in
/// its workspaces.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Running {
    /// They keep it from moving.
    Refused,
    /// The caller stops them before the move, so they and their processes
    /// do not count.
    StoppedFirst,
}

/// A chat without a repository: its ID and name, the `root_dir` it runs
/// in, its temporary directory, attached files and the session files an
/// import kept for it, where it has them, as they resolve before the root
/// changes, and the commands and agents running in it, which must stop
/// before its files move.
pub struct OutsideChat {
    pub id: SpaceId,
    pub name: SpaceName,
    pub root_dir: PathBuf,
    pub tmp: Option<PathBuf>,
    pub files: Option<PathBuf>,
    pub sessions: Option<PathBuf>,
    pub running: Vec<Execution>,
}
