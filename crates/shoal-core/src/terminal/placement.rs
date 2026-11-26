//! Whether a launch takes the caller's terminal or runs in the background.
use crate::config::{Effective, modes::ModeName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Focus,
    Background,
}

impl Placement {
    /// Where a launch in `mode` goes under `settings`: where it was
    /// `requested`, else as its `focus` setting says, else focused when it
    /// waits for the user.
    pub fn of(
        settings: &Effective,
        mode: &ModeName,
        requested: Option<Self>,
        waits_for_user: bool,
    ) -> Self {
        requested.unwrap_or_else(|| Self::choose(settings.focus(mode), waits_for_user))
    }

    /// The `focus` setting, otherwise focus a launch that waits for the user.
    fn choose(setting: Option<bool>, waits_for_user: bool) -> Self {
        if setting.unwrap_or(waits_for_user) {
            Self::Focus
        } else {
            Self::Background
        }
    }
}

#[cfg(test)]
mod tests;
