//! The hook an agent runs before and after its tool calls, which reports
//! what each changes and tells the agent about the changes Shoal could not
//! tell were its own.
use anyhow::Result;
use shoal_protocol::changes::Change;

use crate::{ai::IntegrationKind, cli::context::Context, client::changes};

/// Report what the tool call whose hook event stdin carries changed, as an
/// agent with `integration` names it, and print the changes Shoal could
/// not tell were the agent's as context for it.
pub(in crate::cli::commands) async fn report(
    ctx: &Context,
    integration: IntegrationKind,
) -> Result<i32> {
    let Some(event) = shoal_workers::agent_hooks::input::event() else {
        return Ok(0);
    };
    let Some(change) = crate::cli::agents::tool_change(integration, &event) else {
        return Ok(0);
    };
    let mixed = changes::record(&ctx.paths, change).await?;
    if !mixed.is_empty() {
        let context = question(&mixed);
        outln!(
            "{}",
            super::super::alerts::delivery::hook_output(&event.to_string(), &context)
        );
    }
    Ok(0)
}

/// What the agent is asked about the `mixed` changes: whether it made them.
/// Nothing waits on the answer.
fn question(mixed: &[Change]) -> String {
    let mut text = String::from(
        "Someone else changed these files while your command ran, so Shoal cannot tell whether these changes are yours:",
    );
    for change in mixed {
        text.push_str(&format!("\n  {} {}", change.number, change.place()));
    }
    let numbers: Vec<String> = mixed
        .iter()
        .map(|change| change.number.to_string())
        .collect();
    text.push_str(&format!(
        "\nRun `shoal changes claim {}` with those you made and `shoal changes unclaim` with the others; unanswered ones stay uncertain.",
        numbers.join(" ")
    ));
    text
}
