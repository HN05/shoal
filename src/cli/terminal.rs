//! Where Shoal starts work beside the caller's terminal. Commands choose a
//! placement; a backend carries it out with its own primitives: Herdr tabs or
//! zmx sessions.
use std::{ffi::OsString, path::Path};

use anyhow::{Result, ensure};

use super::{
    commands::{issues::Issue, workspaces::AddPlan},
    context::Context,
    herdr, zmx,
};
use crate::{config::Effective, env, model::Workspace};

/// Whether a launch takes the caller's terminal or runs in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Placement {
    Focus,
    Background,
}

impl Placement {
    /// The `focus` setting, otherwise focus a launch that waits for the user.
    pub fn choose(setting: Option<bool>, waits_for_user: bool) -> Self {
        if setting.unwrap_or(waits_for_user) {
            Self::Focus
        } else {
            Self::Background
        }
    }
}

/// A terminal backend that starts Shoal work beside the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Backend {
    Herdr,
    Zmx,
}

impl Backend {
    /// The backend of this interactive terminal under `settings`: an enabled
    /// Herdr pane.
    pub fn current(ctx: &Context, settings: &Effective) -> Option<Self> {
        (herdr::available(ctx) && settings.herdr.enabled).then_some(Self::Herdr)
    }

    /// The backend of the terminal that queued an entry, from the Herdr
    /// variables the daemon gave its worker; the worker has no terminal.
    pub fn queued(settings: &Effective) -> Self {
        if herdr::in_herdr() && settings.herdr.enabled {
            Self::Herdr
        } else {
            Self::Zmx
        }
    }

    /// Start `plan` as `placement` asks, returning whether it left this
    /// process. A focused zmx launch stays here: its agent attaches to its
    /// session. Herdr tabs need `herdr.new_tab`; `name` is the planned
    /// workspace name that names a session.
    pub async fn start_add(
        self,
        ctx: &Context,
        settings: &Effective,
        plan: &mut AddPlan,
        issue: Option<&Issue>,
        name: &str,
        placement: Placement,
    ) -> Result<bool> {
        ensure!(
            !env::inherits_scope(&ctx.paths.state),
            "workspace processes cannot allocate workspaces"
        );
        match (self, placement) {
            (Self::Herdr, placement) => {
                herdr::start_add(ctx, settings, plan, issue, placement).await
            }
            (Self::Zmx, Placement::Focus) => Ok(false),
            (Self::Zmx, Placement::Background) => {
                zmx::start_add(ctx, plan, name).await?;
                Ok(true)
            }
        }
    }

    /// Run Shoal with `args` for `workspace` in the background, returning
    /// whether it started there; Herdr tabs need `herdr.new_tab`.
    pub async fn run_in_background(
        self,
        ctx: &Context,
        settings: &Effective,
        workspace: &Workspace,
        args: &[OsString],
    ) -> Result<bool> {
        match self {
            Self::Herdr => herdr::run_in_background(ctx, settings, workspace, args).await,
            Self::Zmx => {
                zmx::run_in_background(ctx, workspace, args).await?;
                Ok(true)
            }
        }
    }

    /// Run Shoal with `args` where the agent of `record` stopped, when that
    /// place is another one still open and idle. Returns whether it ran.
    pub async fn rerun_where_stopped(
        self,
        ctx: &Context,
        record: &Path,
        args: &[OsString],
    ) -> Result<bool> {
        match self {
            Self::Herdr => herdr::rerun_in_recorded_pane(ctx, record, args).await,
            // A session ends with its agent, so nothing remains to reuse.
            Self::Zmx => Ok(false),
        }
    }

    /// What several background starts run in.
    pub fn background_places(self) -> &'static str {
        match self {
            Self::Herdr => "Herdr tabs",
            Self::Zmx => "detached zmx sessions",
        }
    }

    /// How a background start is reported: where it runs and how to reach it.
    pub fn background_note(self, name: &str) -> String {
        match self {
            Self::Herdr => format!("Started {name} in a background Herdr tab"),
            Self::Zmx => format!(
                "Started {name} in a detached zmx session; attach with `shoal attach {name}`"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_follows_the_setting_then_whether_the_launch_waits() {
        assert_eq!(Placement::choose(None, true), Placement::Focus);
        assert_eq!(Placement::choose(None, false), Placement::Background);
        assert_eq!(Placement::choose(Some(false), true), Placement::Background);
        assert_eq!(Placement::choose(Some(true), false), Placement::Focus);
    }
}
