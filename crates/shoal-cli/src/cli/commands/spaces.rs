//! CLI workflows that act on any space, a workspace or a chat; all state
//! mutations go through the daemon.
mod list;
mod open;
mod remove;

pub(super) use list::list;
pub(super) use open::open;
pub(super) use remove::remove;
