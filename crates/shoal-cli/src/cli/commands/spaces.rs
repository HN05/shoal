//! CLI workflows that act on any space, a workspace or a chat; all state
//! mutations go through the daemon.
mod done;
mod list;
mod open;
mod remove;
mod rename;
mod status;
mod watch;

pub(super) use done::{done, undone};
pub(super) use list::list;
pub(super) use open::{open, opening};
pub(super) use remove::remove;
pub(super) use rename::{asking, rename, rename_picked, renaming, request_name};
pub(super) use status::status;
pub(super) use watch::watch_items;
