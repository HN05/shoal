//! Local reviews of workspaces and PRs: queueing them, running each once in
//! a detached checkout, and recording and delivering what the reviewer found.
pub(crate) mod store;
