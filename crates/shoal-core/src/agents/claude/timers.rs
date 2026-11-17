//! Claude Code's own timer tools, which keep their timers in the agent's
//! process: they end when Shoal stops the agent, so a hook refuses them and
//! names `shoal sleep`'s timer, which Shoal keeps.
use serde_json::{Value, json};

/// The timer tools, as a hook matcher.
const TOOLS: &str = "ScheduleWakeup|CronCreate|CronDelete";

/// Why a timer tool is refused, and what to use instead.
const REASON: &str = "Shoal stops idle agents, which ends the timers this tool \
    keeps. Run `shoal sleep --for <duration> -- <prompt>` (or `--until HH:MM`) \
    instead: Shoal sends the prompt when it is due, waking the agent if it \
    sleeps. A loop sets its next timer at the end of each turn.";

/// The group that refuses each timer tool before it runs, from its name
/// alone, which the matcher reads, without running Shoal.
pub(super) fn refused() -> (&'static str, Value) {
    let decision = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": REASON,
    }});
    let group = json!({
        "matcher": TOOLS,
        "hooks": [{
            "type": "command",
            "command": format!("printf '%s\\n' {}", crate::shell::quote(&[decision.to_string()])),
            "timeout": 10,
        }],
    });
    ("PreToolUse", group)
}
