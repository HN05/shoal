//! What kind of command a tracked execution runs, which decides its streams,
//! its daemon record and what happens when it stops.
use std::path::PathBuf;

use crate::daemon::workspace::ExecutionKind;

#[derive(Clone)]
pub(super) enum Mode {
    /// An arbitrary command chosen by the caller; `record` keeps it for
    /// `shoal resume` to report when `shoal stop` interrupts it.
    Command {
        record: bool,
    },
    /// A restored agent session; `records` are consumed once it starts.
    Recovery {
        records: Vec<PathBuf>,
    },
    /// A caller-chosen command without a terminal: no stdin, output appended
    /// to `log`, and the launch reported on stdout for the spawning CLI.
    Detached {
        log: PathBuf,
    },
    Land {
        json: bool,
        push: bool,
    },
    /// A command publishing for the workspace under its agent account, with
    /// output on stderr so stdout stays for the caller's result.
    Publish,
    /// The repository's configured setup command.
    Setup,
    /// A demo, restarted whenever its command exits until it is stopped.
    Demo {
        launch: Box<crate::demos::Launch>,
    },
}

impl Mode {
    pub(super) fn kind(&self) -> ExecutionKind {
        match self {
            Self::Command { .. }
            | Self::Recovery { .. }
            | Self::Detached { .. }
            | Self::Publish
            | Self::Demo { .. } => ExecutionKind::Command,
            Self::Land { .. } => ExecutionKind::Land,
            Self::Setup => ExecutionKind::Setup,
        }
    }

    pub(super) fn is_setup(&self) -> bool {
        matches!(self, Mode::Setup)
    }

    /// Commands keep running across a daemon restart; setup and landing
    /// depend on gates the daemon holds.
    pub(super) fn reattaches(&self) -> bool {
        self.kind() == ExecutionKind::Command
    }
}
