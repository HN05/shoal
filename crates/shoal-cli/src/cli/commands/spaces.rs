//! CLI workflows that act on any space, a workspace or a chat; all state
//! mutations go through the daemon.
mod open;

pub(super) use open::open;
