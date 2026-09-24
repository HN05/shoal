//! Shared semantic terminal styles. Keep styling at presentation sites: records,
//! JSON, shell scripts, and child-process output must never acquire ANSI codes.
use std::{
    ffi::OsStr,
    fmt::Display,
    io::{self, IsTerminal},
};

use crate::{
    sim::SimulatorState,
    state::{ExecutionState, WorkspaceState},
};

#[derive(Clone, Copy)]
pub enum Style {
    Heading,
    Success,
    Warning,
    Error,
    Muted,
}

#[derive(Clone, Copy)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    pub fn stdout(json: bool) -> Self {
        Self::detect(io::stdout().is_terminal(), json)
    }

    pub fn stderr(json: bool) -> Self {
        Self::detect(io::stderr().is_terminal(), json)
    }

    fn detect(terminal: bool, json: bool) -> Self {
        Self::for_environment(
            terminal,
            json,
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("TERM").as_deref(),
        )
    }

    fn for_environment(
        terminal: bool,
        json: bool,
        no_color: Option<&OsStr>,
        term: Option<&OsStr>,
    ) -> Self {
        Self {
            enabled: terminal
                && !json
                && !no_color.is_some_and(|value| !value.is_empty())
                && term != Some(OsStr::new("dumb")),
        }
    }

    pub fn paint(self, style: Style, value: impl Display) -> String {
        if !self.enabled {
            return value.to_string();
        }
        let code = match style {
            Style::Heading => "1;36",
            Style::Success => "32",
            Style::Warning => "33",
            Style::Error => "1;31",
            Style::Muted => "2",
        };
        format!("\x1b[{code}m{value}\x1b[0m")
    }

    pub fn workspace_state(self, state: WorkspaceState) -> String {
        let style = match state {
            WorkspaceState::Ready => Style::Success,
            WorkspaceState::Failed => Style::Error,
            WorkspaceState::Preparing
            | WorkspaceState::Stopping
            | WorkspaceState::Removing
            | WorkspaceState::Reconciling => Style::Warning,
        };
        self.paint(style, state)
    }

    pub fn simulator_state(self, state: SimulatorState) -> String {
        self.paint(
            match state {
                SimulatorState::Leased => Style::Success,
                SimulatorState::Idle => Style::Muted,
                SimulatorState::Creating | SimulatorState::Booting => Style::Warning,
                SimulatorState::Failed => Style::Error,
            },
            state,
        )
    }

    pub fn execution_state(self, state: ExecutionState) -> String {
        self.paint(
            match state {
                ExecutionState::Running => Style::Success,
                ExecutionState::Unknown => Style::Warning,
            },
            state,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_requires_a_human_terminal_without_an_opt_out() {
        for (terminal, json, no_color, term, enabled) in [
            (true, false, None, None, true),
            (false, false, None, None, false),
            (true, true, None, None, false),
            (true, false, Some("1"), None, false),
            (true, false, Some("0"), None, false),
            (true, false, Some(""), None, true),
            (true, false, None, Some("dumb"), false),
            (true, false, None, Some("xterm-256color"), true),
        ] {
            let palette = Palette::for_environment(
                terminal,
                json,
                no_color.map(OsStr::new),
                term.map(OsStr::new),
            );
            assert_eq!(palette.enabled, enabled);
            let output = palette.paint(Style::Error, "error:");
            assert_eq!(output.contains('\x1b'), enabled);
            if !enabled {
                assert_eq!(output, "error:");
            }
        }
    }

    #[test]
    fn styles_reset_and_state_labels_keep_their_wire_spelling() {
        let palette = Palette { enabled: true };
        assert_eq!(
            palette.workspace_state(WorkspaceState::Ready),
            "\x1b[32mready\x1b[0m"
        );
        assert_eq!(
            palette.workspace_state(WorkspaceState::Failed),
            "\x1b[1;31mfailed\x1b[0m"
        );
        assert_eq!(
            palette.execution_state(ExecutionState::Unknown),
            "\x1b[33munknown\x1b[0m"
        );
        let plain = Palette { enabled: false };
        assert_eq!(
            plain.workspace_state(WorkspaceState::Preparing),
            "preparing"
        );
        assert_eq!(
            serde_json::to_string(&WorkspaceState::Ready).unwrap(),
            "\"ready\""
        );
    }
}
