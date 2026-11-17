//! How the CLI colors the status column of workspace and agent queue
//! lists, and the marker that starts each row.
use crate::cli::output::{Palette, Style};
pub use shoal_protocol::list_status::{Group, ListStatus, marks_wait, queued_in};

/// How a [`ListStatus`] shows in the terminal.
pub trait ListStatusStyle {
    /// The color of the status column.
    fn style(self) -> Style;

    /// A one-column marker for what the status asks of the user, whose
    /// shape stays readable without color.
    fn marker(self, palette: Palette) -> String;
}

impl ListStatusStyle for ListStatus {
    fn style(self) -> Style {
        match self {
            Self::Failed => Style::Error,
            Self::Ready => Style::Success,
            Self::Finished => Style::Done,
            Self::Working => Style::Heading,
            Self::Waiting | Self::Asleep => Style::Idle,
            Self::ProcessRunning | Self::ParentWorking => Style::Active,
            Self::NeedsInput => Style::Attention,
            Self::Inactive | Self::Queued => Style::Muted,
            Self::Preparing
            | Self::Stopping
            | Self::Removing
            | Self::Reconciling
            | Self::Restoring
            | Self::Updating
            | Self::Stopped
            | Self::Blocked
            | Self::Starting => Style::Warning,
        }
    }

    fn marker(self, palette: Palette) -> String {
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
