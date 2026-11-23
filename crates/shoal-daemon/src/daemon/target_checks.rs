//! What each request requires of the space it acts on: every request group
//! declares its check with no default arm, and the spaces service judges
//! the space by it before the request's handler.
use shoal_protocol::protocol::Operation;

pub(crate) use shoal_spaces::target_checks::*;

/// A request that declares the check of the space it acts on.
pub(crate) trait Checked {
    /// The check of the space the request names, as its target gives it;
    /// `None` for a request kind that never names one.
    fn target_check(&self) -> Option<TargetCheck>;
}

impl Checked for Operation {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::Daemon(method) => method.target_check(),
            Self::Repository(method) => method.target_check(),
            Self::Workspace(method) => method.target_check(),
            Self::Space(method) => method.target_check(),
            Self::Place(method) => method.target_check(),
            Self::Permission(method) => method.target_check(),
            Self::Config(method) => method.target_check(),
            Self::Diff(method) => method.target_check(),
            Self::Merging(method) => method.target_check(),
            Self::Removal(method) => method.target_check(),
            Self::Recovery(method) => method.target_check(),
            Self::Execution(method) => method.target_check(),
            Self::Assignment(method) => method.target_check(),
            Self::Agent(method) => method.target_check(),
            Self::Chat(method) => method.target_check(),
            Self::Launch(method) => method.target_check(),
            Self::Queue(method) => method.target_check(),
            Self::Hold(method) => method.target_check(),
            Self::Demo(method) => method.target_check(),
            Self::File(method) => method.target_check(),
            Self::Item(method) => method.target_check(),
            Self::Ci(method) => method.target_check(),
            Self::Branch(method) => method.target_check(),
            Self::Alert(method) => method.target_check(),
            Self::Conversion(method) => method.target_check(),
            Self::Operations(method) => method.target_check(),
            Self::Thread(method) => method.target_check(),
            Self::Change(method) => method.target_check(),
            Self::Review(method) => method.target_check(),
            Self::Approval(method) => method.target_check(),
            Self::Port(method) => method.target_check(),
            Self::Secret(method) => method.target_check(),
            Self::Resource(method) => method.target_check(),
            Self::Simulator(method) => method.target_check(),
            Self::Container(method) => method.target_check(),
            Self::Subscription(method) => method.target_check(),
            Self::Trigger(method) => method.target_check(),
            Self::Hub(method) => method.target_check(),
            Self::App(method) => method.target_check(),
        }
    }
}
