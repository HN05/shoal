//! The place an asleep agent's wrapper waits in for its restore: on a
//! terminal it holds, a status block under the agent's last output saying
//! why the agent sleeps and what wakes it, which reads the keys that wake or
//! stop it; elsewhere, lines on standard error.
mod keys;
mod screen;

pub(super) use keys::Key;
use keys::Keys;
use screen::{Screen, Wake};

use crate::model::Protection;

pub(in super::super) struct Place(Shown);

enum Shown {
    /// The status block on a terminal the wrapper holds in the foreground,
    /// boxed since the terminal settings `keys` restores make the variant
    /// large on macOS.
    Terminal { screen: Box<Screen>, keys: Keys },
    /// Lines on standard error, which reads no keys.
    Lines,
}

impl Place {
    /// Show the agent asleep until `protection` lets it resume.
    pub(super) fn show(protection: &Protection) -> Self {
        let Some(keys) = Keys::open() else {
            eprintln!(
                "shoal: agent stopped: {}; waiting until {}; the agent resumes automatically",
                protection.reason, protection.resumes_when
            );
            return Self(Shown::Lines);
        };
        let mut screen = Box::new(Screen::new(protection));
        screen.open();
        Self(Shown::Terminal { screen, keys })
    }

    /// Lines on standard error, as without a terminal.
    #[cfg(test)]
    pub(in super::super) fn lines() -> Self {
        Self(Shown::Lines)
    }

    /// The next key that decides the wait; never without a terminal.
    pub(super) async fn key(&mut self) -> Key {
        match &mut self.0 {
            Shown::Terminal { keys, .. } => keys.next().await,
            Shown::Lines => std::future::pending().await,
        }
    }

    /// Tell the user what else the wait depends on.
    pub(super) fn note(&mut self, line: String) {
        match &mut self.0 {
            Shown::Terminal { screen, .. } => screen.note(line),
            Shown::Lines => eprintln!("shoal: {line}"),
        }
    }

    /// Show that the agent wakes now.
    pub(super) fn waking(&mut self) {
        self.show_wake(Wake::Waking);
    }

    /// Show that the wake could not reach the daemon because of `error`.
    pub(super) fn wake_unsent(&mut self, error: &anyhow::Error) {
        self.show_wake(Wake::Unsent(format!("{error:#}")));
    }

    fn show_wake(&mut self, wake: Wake) {
        if let Shown::Terminal { screen, .. } = &mut self.0 {
            screen.wake(wake);
        }
    }
}
