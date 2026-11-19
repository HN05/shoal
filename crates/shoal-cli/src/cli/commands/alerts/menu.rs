//! The picker `shoal alert list` opens on a terminal: one row per alert,
//! where a key binding chooses what to do with it.
use anyhow::{Context as _, Result};

use super::{cells, shown, withdraw};
use crate::cli::{
    context::Context,
    output::Palette,
    ui::{self, Key, KeyBindings, WidthLimit},
};
use shoal_protocol::alerts::Alert;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AlertAction {
    Print,
    Withdraw,
}

const BINDINGS: &[(Key, &str, AlertAction)] = &[
    (Key::Enter, "print", AlertAction::Print),
    (Key::Ctrl('w'), "withdraw", AlertAction::Withdraw),
];

/// Pick one of `alerts` and act on it.
pub(super) async fn choose(ctx: &Context, alerts: &[Alert]) -> Result<i32> {
    if alerts.is_empty() {
        outln!("No alerts");
        return Ok(0);
    }
    let picked = ui::pick_with_keys(ctx, "Alert> ", entries(alerts), KeyBindings(BINDINGS))?;
    let alert = (alerts.iter())
        .find(|alert| alert.id.to_string() == picked.id)
        .context("picker returned an unknown alert")?;
    match picked.action {
        AlertAction::Print => {
            outln!("{}", shown(alert));
            Ok(0)
        }
        AlertAction::Withdraw => withdraw(ctx, alert.id).await,
    }
}

/// One row per alert, newest first, cut to fit the picker.
fn entries(alerts: &[Alert]) -> ui::Entries {
    let now = crate::time::UnixTime::now();
    let newest: Vec<&Alert> = alerts.iter().rev().collect();
    let rows = newest.iter().map(|alert| cells(alert, now)).collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 4, width });
    let labels = ui::aligned_to(rows, Palette::picker(), limit);
    (newest.iter().map(|alert| alert.id.to_string()))
        .zip(labels)
        .collect()
}
