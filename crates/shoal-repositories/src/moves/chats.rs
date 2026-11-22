//! The chats without a repository whose files `shoal repo move` moves into
//! the directory that holds the repositories once it is done, as the
//! daemon's chats find them before they move them.
use std::path::PathBuf;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{model::Execution, space_names::SpaceName};

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
