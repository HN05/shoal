//! The picker's views, with the keys each binds.
use super::keys::Action;
use crate::cli::ui::PickerView;

/// What the picker lists: what needs the user, the work going on, or what
/// the daemon saw.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum View {
    Inbox,
    Spaces,
    Notifications,
}

impl View {
    pub(super) fn prompt(self) -> &'static str {
        match self {
            Self::Inbox => "inbox> ",
            Self::Spaces => "spaces> ",
            Self::Notifications => "notifications> ",
        }
    }

    /// The actions the view binds, in header order.
    fn binds(self, action: Action) -> bool {
        match self {
            Self::Inbox => !matches!(action, Action::Execute | Action::Stop | Action::Delete),
            Self::Notifications => {
                matches!(action, Action::Enter | Action::Status | Action::Add)
            }
            Self::Spaces => true,
        }
    }

    pub(super) fn enter_label(self) -> &'static str {
        match self {
            Self::Inbox => "act",
            Self::Spaces | Self::Notifications => "cd",
        }
    }

    /// The views a caller with this scope can switch between; workspace
    /// processes cannot decide what needs the user or read notifications.
    pub(super) fn available(scoped: bool) -> &'static [Self] {
        if scoped {
            &[Self::Spaces]
        } else {
            &[Self::Inbox, Self::Spaces, Self::Notifications]
        }
    }

    /// The actions a caller with this scope may use here.
    fn bound(self, scoped: bool) -> impl Iterator<Item = Action> {
        Action::ALL
            .into_iter()
            .filter(move |&action| self.binds(action) && (!scoped || action.allowed_scoped()))
    }

    /// The view's presentation and keys for the shared picker, naming Tab
    /// when it `switches` to another view.
    pub(super) fn description(self, scoped: bool, switches: bool) -> PickerView<'static> {
        let keys: Vec<_> = self
            .bound(scoped)
            .map(|action| (action.key(), action.label(self)))
            .collect();
        PickerView::new(self.prompt(), &keys, switches)
    }
}
