//! The Shoal integrations an AI tool runs with, which agents' hooks and
//! their sessions' files follow.

shoal_states::states!(
    /// The Shoal integration a tool runs with: that of the built-in tool its
    /// lineage ends at. Agent hooks name it to the commands they run.
    IntegrationKind: ValueEnum {
        Claude => "claude",
        Codex => "codex",
        /// Configuration says everything about the tool.
        None => "none",
    }
);

impl IntegrationKind {
    /// Whether Shoal's hooks report the end of each turn of its agents,
    /// with the turn's final message.
    pub fn reports_turn_ends(self) -> bool {
        self != Self::None
    }
}
