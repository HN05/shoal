//! An agent session's ID: the one the agent gives its session, not one the
//! daemon gives a record.

crate::macros::text_id!(
    /// The ID an agent gives its session, which its hooks report and its
    /// resume and fork arguments name; never a terminal host's session.
    AgentSessionId
);
