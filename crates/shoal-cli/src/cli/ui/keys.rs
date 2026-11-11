//! The keys pickers bind, and fzf's names for them.

/// A key a picker binds to an action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Enter,
    /// Ctrl with a lowercase letter.
    Ctrl(char),
}

impl Key {
    /// Its fzf name, which key hints show too.
    pub(super) fn name(self) -> String {
        match self {
            Self::Enter => "enter".into(),
            Self::Ctrl(letter) => format!("ctrl-{letter}"),
        }
    }

    /// The hint a header shows for it: its name and what it does.
    pub(super) fn hint(self, label: &str) -> String {
        format!("{}: {label}", self.name())
    }

    /// Whether fzf reported pressing it as `name`; a picker that prints no
    /// key was left with Enter.
    pub(super) fn reported_as(self, name: &str) -> bool {
        match name {
            "" => self == Self::Enter,
            name => self.name() == name,
        }
    }
}

#[cfg(test)]
mod tests;
