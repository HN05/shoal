//! Shared semantic terminal styles. Keep styling at presentation sites: records,
//! JSON, shell scripts, and child-process output must never acquire ANSI codes.
use std::{
    ffi::OsStr,
    fmt::Display,
    io::{self, IsTerminal},
    os::fd::{AsFd, AsRawFd},
};

use crate::{
    daemon::refusal::Reason,
    forge::review::status::ReviewState,
    protocol::{ErrorCode, RemoteError},
    sim::SimulatorState,
    state::{ExecutionState, WorkspaceState},
};

/// The columns of the terminal `fd` writes to, or `None` when it is not one.
pub fn terminal_width(fd: impl AsFd) -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one winsize into `size` for an open descriptor.
    let status = unsafe {
        libc::ioctl(
            fd.as_fd().as_raw_fd(),
            libc::TIOCGWINSZ as _,
            &mut size as *mut libc::winsize,
        )
    };
    (status == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// The width text rows printed to stdout should fit: the terminal's, and
/// none for JSON or output that is not a terminal.
pub fn stdout_width(json: bool) -> Option<usize> {
    if json {
        return None;
    }
    terminal_width(io::stdout())
}

/// Whether text printed to stdout may use emoji: a terminal, not JSON, in a
/// UTF-8 locale and not one known to lack emoji.
pub fn stdout_shows_emoji(json: bool) -> bool {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty());
    shows_emoji(
        io::stdout().is_terminal() && !json,
        std::env::var_os("TERM").as_deref(),
        locale.as_deref(),
    )
}

/// The first locale variable set decides the character set, as in libc; the
/// Linux console and dumb terminals have no emoji glyphs.
fn shows_emoji(terminal: bool, term: Option<&OsStr>, locale: Option<&OsStr>) -> bool {
    let utf8 = locale.is_some_and(|locale| {
        let locale = locale.to_string_lossy().to_ascii_lowercase();
        locale.contains("utf-8") || locale.contains("utf8")
    });
    terminal && utf8 && !term.is_some_and(|term| term == "dumb" || term == "linux")
}

#[derive(Clone, Copy)]
pub enum Style {
    Heading,
    Success,
    Warning,
    Error,
    Muted,
    /// Something waits for the user.
    Attention,
    /// Something runs without saying what it does.
    Active,
    /// An agent waits for a turn.
    Idle,
    /// Work is complete and needs nothing more.
    Done,
}

/// A failure as JSON output reports it: under the daemon's code when it has a
/// specific one, or the code of a refusal the CLI raised, otherwise
/// `command_failed`.
pub fn json_error(error: &anyhow::Error) -> serde_json::Value {
    let code = error
        .downcast_ref::<RemoteError>()
        .map(|remote| remote.code.clone())
        .or_else(|| Reason::of(error).map(Reason::code))
        .filter(|code| *code != ErrorCode::OperationFailed)
        .map_or_else(|| "command_failed".to_owned(), |code| code.to_string());
    serde_json::json!({"error": {"code": code, "message": format!("{error:#}")}})
}

/// Print the help or version text clap reports as a parse error, which clap's
/// own printing would not end with the status a closed stdout gets.
pub fn print_clap_output(error: &clap::Error) {
    out!("{}", Palette::stdout(false).styled(&error.render()));
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

    /// For rows a picker shows on the terminal, wherever they are written.
    pub fn picker() -> Self {
        Self::detect(true, false)
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

    /// Clap's styled text, such as help, with its styles only when enabled.
    pub fn styled(self, text: &clap::builder::StyledStr) -> String {
        if self.enabled {
            text.ansi().to_string()
        } else {
            text.to_string()
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
            Style::Attention => "1;33",
            Style::Active => "36",
            Style::Idle => "34",
            Style::Done => "2;32",
        };
        format!("\x1b[{code}m{value}\x1b[0m")
    }

    pub fn workspace_state(self, state: WorkspaceState) -> String {
        self.paint(workspace_state_style(state), state)
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

    pub fn review_state(self, state: ReviewState) -> String {
        self.paint(
            match state {
                ReviewState::Approved => Style::Success,
                ReviewState::ChangesRequested => Style::Error,
                ReviewState::Commented | ReviewState::Unreviewed => Style::Muted,
            },
            state,
        )
    }

    /// Forges spell check results differently; color the common ones.
    pub fn check_result(self, result: &str) -> String {
        self.paint(
            match result {
                "success" => Style::Success,
                "failure" | "error" | "timed_out" | "action_required" => Style::Error,
                "pending" | "queued" | "in_progress" | "expected" | "waiting" => Style::Warning,
                _ => Style::Muted,
            },
            result,
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

pub fn workspace_state_style(state: WorkspaceState) -> Style {
    match state {
        WorkspaceState::Ready => Style::Success,
        WorkspaceState::Failed => Style::Error,
        WorkspaceState::Preparing
        | WorkspaceState::Stopping
        | WorkspaceState::Removing
        | WorkspaceState::Reconciling
        | WorkspaceState::Restoring
        | WorkspaceState::Updating => Style::Warning,
    }
}

#[cfg(test)]
mod tests;
