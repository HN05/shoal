//! What the daemon gives the agents' service: the spaces agents run in and
//! their settings, the executions that run them, the alerts that tell of
//! them, what else ends with an agent's turn, the agent queue, and the
//! records of other subsystems it reads and writes.
mod alerts;
mod executions;
mod queue;
mod records;
mod spaces;
mod turns;

pub(crate) use records::AgentRecords;

#[cfg(test)]
mod tests;
