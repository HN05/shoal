//! The agent queue's shared logic: what everything that queues an agent
//! start and the worker that carries it out agree on. The protocol calls
//! live in [`crate::client::queue`] and the daemon's runner in its own
//! `agent_queue`.
mod plan;

pub(crate) use plan::{AddPlan, CopyCreation, Creation, ResolvedAddAgent, queued_agent_request};
