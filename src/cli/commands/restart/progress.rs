//! What a run tells the user: a numbered line for each selected agent as
//! its restart ends or is skipped, with or without `--json` the object that
//! lists them.
use anyhow::Result;
use serde_json::{Value, json};

use super::plan::{Restart, Skipped};
use crate::cli::{
    context::Context,
    output::{Palette, Style},
    ui,
};

pub(super) struct Progress {
    report: ui::Report,
    skipped: Vec<Value>,
    /// How many agents the run reported on so far, and how many it selected.
    position: usize,
    total: usize,
}

impl Progress {
    pub(super) fn new(total: usize) -> Self {
        Self {
            report: ui::Report::new("restarted", "restart"),
            skipped: Vec::new(),
            position: 0,
            total,
        }
    }

    /// The next agent's place in the run, as `(n/total)`.
    fn next(&mut self) -> String {
        self.position += 1;
        format!("({}/{})", self.position, self.total)
    }

    /// Report an agent the run does not restart.
    pub(super) fn skip(&mut self, ctx: &Context, skipped: Skipped) {
        let place = self.next();
        if !ctx.json {
            let message = format!("Skipped {} {place}: {}", skipped.workspace, skipped.reason);
            outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
        }
        self.skipped.push(json!({
            "workspace": skipped.workspace,
            "agent": skipped.agent,
            "reason": skipped.reason,
        }));
    }

    /// Report how the restart of `restart` ended.
    pub(super) fn ended(&mut self, ctx: &Context, restart: &Restart, result: Result<()>) {
        let place = self.next();
        let name = &restart.workspace.name;
        match result {
            Ok(()) => self.report.done(
                ctx,
                &format!("Restarted {name} {place}"),
                json!({"workspace": name, "agent": restart.agent.agent}),
            ),
            Err(error) => {
                let message = match super::waits(&error) {
                    Some(waits) => format!("Pending {name} {place}: {waits}"),
                    None => format!("Failed {name} {place}: {error:#}"),
                };
                self.report.failed_saying(ctx, &message, name, &error);
            }
        }
    }

    /// Print the JSON object, exiting 1 when any restart failed.
    pub(super) fn finish(mut self, ctx: &Context) -> Result<i32> {
        self.report.field("skipped", self.skipped.into());
        self.report.finish(ctx, "")
    }
}
