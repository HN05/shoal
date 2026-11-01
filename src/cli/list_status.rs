//! How the CLI colors the status column of workspace and agent queue lists,
//! which [`crate::client::list_status`] labels.
use crate::cli::output::{Palette, Style};
pub(crate) use crate::client::list_status::*;

impl ListStatus {
    pub fn style(self) -> Style {
        match self {
            Self::Failed => Style::Error,
            Self::Ready => Style::Success,
            Self::Finished => Style::Done,
            Self::Working => Style::Heading,
            Self::Waiting => Style::Idle,
            Self::ProcessRunning => Style::Active,
            Self::NeedsInput => Style::Attention,
            Self::Inactive | Self::Queued => Style::Muted,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating
            | Self::Stopped
            | Self::ReadyOutdated
            | Self::Starting => Style::Warning,
        }
    }

    /// A one-column marker for what the status asks of the user, whose shape
    /// stays readable without color.
    pub fn marker(self, palette: Palette) -> String {
        let group = self.group();
        let style = match group {
            Group::YourTurn => Style::Attention,
            Group::Busy => Style::Heading,
            Group::Quiet => Style::Muted,
            Group::Changing => Style::Warning,
            Group::Failed => Style::Error,
        };
        palette.paint(style, group.marker())
    }
}

#[cfg(test)]
mod tests;
