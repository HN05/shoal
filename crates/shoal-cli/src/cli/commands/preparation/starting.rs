//! How a command reports the starts it carries out in its own process:
//! a workspace its preparation readied, and a shell without the
//! integration that follows it into the workspace.
use anyhow::Result;
use shoal_workers::{hosting::Caller, starts::Starting};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        shell,
    },
    model::Workspace,
};

impl Starting for Context {
    fn caller(&self) -> Caller<'_> {
        Context::caller(self)
    }

    fn prepared(&self, workspace: &Workspace, reused: bool) -> Result<()> {
        self.show(workspace, |workspace| {
            outln!(
                "{} {} {}",
                if reused { "Opened" } else { "Created" },
                Palette::stdout(self.json).paint(Style::Heading, &workspace.name),
                super::reports::location_phrase(workspace),
            );
        })
    }

    fn not_integrated(&self) {
        shell::not_integrated(self.json);
    }
}
