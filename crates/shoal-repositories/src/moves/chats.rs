//! The chats without a repository whose files `shoal repo move` moves into
//! the directory that holds the repositories once it is done, as the
//! daemon's chats find them before they move them.
pub use shoal_daemon_base::root_moves::OutsideChat;
