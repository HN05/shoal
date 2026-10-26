//! The terminal a tracked command or interactive hook runs in, handed to it as
//! its foreground job and restored when it exits.
use std::{
    io::{IsTerminal, Write},
    os::unix::fs::OpenOptionsExt,
    process::ExitStatus,
};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

/// Restore foreground ownership, OS settings and shell input modes on exit.
pub(crate) struct Terminal {
    previous_group: i32,
    previous_settings: libc::termios,
    previous_handler: Option<libc::sighandler_t>,
    output: Option<std::fs::File>,
}

impl Terminal {
    pub(crate) fn stdin_is_background() -> bool {
        std::io::stdin().is_terminal()
            && unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) != libc::getpgrp() }
    }

    /// Capture before launching a child. Background hooks skip the terminal;
    /// tracked commands require foreground ownership when stdin is a terminal.
    pub(crate) fn capture(require_foreground: bool) -> Result<Option<Self>> {
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        // SAFETY: tcgetpgrp/getpgrp inspect the calling process and stdin.
        let previous_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous_group != unsafe { libc::getpgrp() } {
            ensure!(
                !require_foreground,
                "cannot hand the terminal to a command unless shoal is a foreground terminal job"
            );
            return Ok(None);
        }
        let mut settings = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes settings on success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, settings.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("capture terminal settings");
        }
        Ok(Some(Self {
            previous_group,
            previous_settings: unsafe { settings.assume_init() },
            previous_handler: None,
            output: Self::input_mode_output(),
        }))
    }

    pub(crate) fn give_to(&mut self, group: i32) -> Result<()> {
        // Ignore SIGTTOU only after spawning, so the child keeps normal job
        // control while the wrapper can reclaim the terminal from background.
        self.previous_handler = Some(unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) });
        // SAFETY: stdin is our foreground terminal, captured before launch.
        ensure!(
            unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, group) } == 0,
            "cannot hand terminal to command"
        );
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: ignore SIGTTOU while reclaiming our terminal from the child's
        // foreground group. TCSANOW restores settings without discarding input
        // or waiting indefinitely for output to drain.
        unsafe {
            let previous_handler = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, self.previous_group);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.previous_settings);
            if self.previous_handler.is_some() {
                self.reset_input_modes();
            }
            libc::signal(
                libc::SIGTTOU,
                self.previous_handler.unwrap_or(previous_handler),
            );
        }
    }
}

impl Terminal {
    fn input_mode_output() -> Option<std::fs::File> {
        let term = std::env::var_os("TERM")?;
        if term.is_empty() || term == "dumb" {
            return None;
        }
        // Use the controlling terminal even when both output streams are
        // redirected. Cleanup must neither enter a pipe nor block on output,
        // and inability to open the output must not prevent command launch.
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open("/dev/tty")
            .ok()
    }

    fn reset_input_modes(&mut self) {
        let Some(output) = &mut self.output else {
            return;
        };
        reset_input_modes_on(output);
    }
}

/// Run `command` in this process's terminal as its foreground job, as an
/// interactive hook runs, then restore the terminal's foreground group, OS
/// settings and emulator input modes, whatever the command left.
pub(crate) async fn run_in_terminal(mut command: Command) -> Result<ExitStatus> {
    let mut terminal = Terminal::capture(false)?;
    let mut child = command.process_group(0).spawn()?;
    let group = child.id().context("command process ID unavailable")? as i32;
    if let Some(terminal) = &mut terminal {
        terminal.give_to(group)?;
    }
    // The command may have stopped on terminal I/O before becoming foreground.
    // SAFETY: signals only the process group spawned above.
    unsafe {
        libc::kill(-group, libc::SIGCONT);
    }
    let status = child.wait().await?;
    drop(terminal);
    Ok(status)
}

/// Reset emulator input modes after a terminal owner returns control to the shell.
pub(crate) fn reset_input_modes() {
    let Some(term) = Terminal::input_mode_output() else {
        return;
    };
    let mut output = term;
    reset_input_modes_on(&mut output);
}

fn reset_input_modes_on(output: &mut std::fs::File) {
    // termios cannot undo emulator modes left by an interrupted TUI.
    // Disable mouse tracking/encodings, focus and paste reports, and restore
    // ordinary cursor/keypad and keyboard input. Avoid a full terminal reset,
    // which would clear the user's scrollback and other terminal settings.
    let _ = output.write_all(
        concat!(
            "\x1b[?1000l\x1b[?1002l\x1b[?1003l",
            "\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l",
            "\x1b[?1004l\x1b[?1007l\x1b[?2004l",
            "\x1b[?1l\x1b>\x1b[>4;0m\x1b[=0u",
        )
        .as_bytes(),
    );
}
