//! Cooperative caller scope: the caller a scope token stands for, the
//! spaces its scope reaches, and what it sees of the rest. The daemon
//! confines every scoped request with them; this is not a security boundary
//! against the OS user.
mod access;
mod reach;
pub mod tokens;

pub use reach::outside_scope;
pub use shoal_daemon_base::callers::{Caller, ScopedExecution};
