//! Triggers: `[triggers.NAME]` config that starts agents when an event
//! source fires, and the runtime state the daemon keeps for each.
use shoal_states::states;

states!(
    /// Where a trigger's firings come from.
    SourceKind: Variants {
        /// Clock times or an interval.
        Timer => "timer",
    }
);
