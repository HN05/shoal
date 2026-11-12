//! Restarting several agents, of workspaces and of chats alike, each so it
//! runs its agent's current binary: which ones a run restarts.
mod plan;

pub use plan::{
    Plan, Restart, Selection, running_agents, skip_reason, space_skip_reason, stopped_reason,
};
