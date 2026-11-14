//! The process group of a wrapper that runs another space's agent in the
//! process that started it, such as a scoped caller's `start --here`: its
//! starter's group is the one a stop of the starter's execution signals.
use std::io::IsTerminal;

use anyhow::{Result, ensure};

use super::terminal::take_foreground;

/// This process, moved out of its starter's process group into one of its
/// own; dropping it gives the terminal back to the starter's group when
/// this process took it from that group.
pub(super) struct OwnGroup {
    /// The starter's group, when it held the terminal this process took.
    returns_terminal_to: Option<i32>,
}

impl OwnGroup {
    /// Lead a process group of its own unless this process already leads
    /// one, keeping the terminal on standard input when its starter's group
    /// held it.
    pub(super) fn lead() -> Result<Self> {
        // SAFETY: getpgrp, getpid and tcgetpgrp only inspect this process
        // and its standard input.
        let starter = unsafe { libc::getpgrp() };
        if starter == unsafe { libc::getpid() } {
            return Ok(Self {
                returns_terminal_to: None,
            });
        }
        let foreground = std::io::stdin().is_terminal()
            && unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) } == starter;
        // SAFETY: setpgid(0, 0) moves only this process, which leads no
        // group and so no session.
        ensure!(
            unsafe { libc::setpgid(0, 0) } == 0,
            "leave the starter's process group: {}",
            std::io::Error::last_os_error()
        );
        if foreground {
            take_foreground();
        }
        Ok(Self {
            returns_terminal_to: foreground.then_some(starter),
        })
    }
}

impl Drop for OwnGroup {
    fn drop(&mut self) {
        let Some(group) = self.returns_terminal_to else {
            return;
        };
        // SAFETY: ignore SIGTTOU while handing this terminal back from this
        // process's group, which may be in the background by now.
        unsafe {
            let previous = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, group);
            libc::signal(libc::SIGTTOU, previous);
        }
    }
}
