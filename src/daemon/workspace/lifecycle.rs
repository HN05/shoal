//! One removal path for explicit removal and automatic cleanup: [`policy`]
//! decides what each removal checks, [`candidacy`] what keeps a workspace
//! from cleanup, and [`remove`] carries removal out, which [`background`]
//! lets a request leave running; [`conversion`] removes the worktree of a
//! workspace that becomes a chat along the same path.
mod background;
mod candidacy;
mod chats;
mod conversion;
mod policy;
mod remove;
#[cfg(test)]
mod tests;

use policy::{Removal, Retention, Stage};
