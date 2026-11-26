//! `shoal internal change`: the hook an agent runs around its tool calls to
//! report what each changes.
use anyhow::Result;
use shoal_protocol::changes::Change;

use crate::{agents::integration, ai::IntegrationKind, client::connection::CommandPaths};

/// Report what the tool call whose hook event stdin carries changed, as an
/// agent with `integration` names it, and print the changes Shoal could
/// not tell were the agent's as context for it.
pub async fn report(paths: &CommandPaths, integration: IntegrationKind) -> Result<i32> {
    let Some(event) = super::input::event() else {
        return Ok(0);
    };
    let Some(change) = self::integration(integration).tool_change(&event) else {
        return Ok(0);
    };
    let mixed = crate::client::changes::record(paths, change).await?;
    if !mixed.is_empty() {
        let context = question(&mixed);
        shoal_core::outln!("{}", super::output(&event.to_string(), &context));
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
