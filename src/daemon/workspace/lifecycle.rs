//! One removal path for explicit removal and automatic cleanup: [`policy`]
//! decides what each removal checks, [`candidacy`] what keeps a workspace
//! from cleanup, and [`remove`] carries removal out.
mod candidacy;
mod chats;
mod policy;
mod remove;
#[cfg(test)]
mod tests;

use policy::{Removal, Retention, Stage};
