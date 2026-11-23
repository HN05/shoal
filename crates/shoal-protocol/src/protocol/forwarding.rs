//! Which requests a daemon forwards through the hub when the space they act
//! on belongs to another joined machine, decided for every request kind
//! with no default. One that reads or changes Shoal's state goes to the
//! space's machine, which decides it as its own caller's, and one whose
//! answer goes on, as a follow's, comes back on a stream, and one whose
//! connection carries bytes both ways, as a demo's, on a duplex stream; only
//! one that needs the caller's machine stays there.
use crate::{
    approvals::ApprovalRequest, hub::directory::Keeps, item_numbers::ItemNumber,
    space_names::SpaceSelector, threads::ThreadNumber,
};

use super::{
    AlertMethod, ApprovalMethod, ConversionMethod, ItemContext, LaunchMethod, Operation,
    OperationMethod, RemovalMethod, RepoTarget,
};

/// What a daemon does with a request for another machine's space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forwarding {
    /// It needs the caller's machine: the caller's execution, terminal or
    /// files, a report of its agent's hooks or wrapper, a lease taken for
    /// it, or a space created or listed there. A request for another
    /// machine's space is refused, naming that machine.
    Never,
    /// It goes to the space's machine, and fails at once while that machine
    /// is offline.
    Online,
    /// It goes to the space's machine, and while that machine is offline
    /// is answered with what the machine last reported to the hub, as a
    /// space's status is.
    Reported,
    /// It goes to the space's machine, and waits at the hub while that
    /// machine is offline, as messages, answers, decisions and stops can.
    Queued,
    /// It goes to the space's machine, whose answer comes back on a stream
    /// as that machine writes it, as a follow's does; it fails at once while
    /// that machine is offline.
    Streamed,
    /// It goes to the space's machine, whose answer comes back on a stream
    /// as for [`Self::Streamed`], and after the answer's first frame the
    /// caller's bytes go there on the same stream, as a demo's and carried
    /// commits' do; it fails at once while that machine is offline.
    Duplex,
}

/// What a request acts on, which decides the machine that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// A space, a workspace or a chat.
    Space(&'a SpaceSelector),
    /// A space's attached files, which outlive it: a removed space's are
    /// read where a machine keeps them.
    Files(&'a SpaceSelector),
    /// A space, or once it is removed, its removal record, which is read
    /// where a machine keeps it.
    Removable(&'a SpaceSelector),
    /// A thread or an approval request, numbered in the space its number
    /// begins with.
    Item(&'a ItemNumber),
    /// The space named, for a request from the user; a space's process
    /// acts in its own space, as a thread it opens is numbered there.
    Acting(&'a SpaceSelector),
}

impl RepoTarget {
    /// The space the request acts in, if it names one.
    pub fn named_space(&self) -> Option<&SpaceSelector> {
        self.context.as_ref().and_then(ItemContext::named_space)
    }
}

impl ItemContext {
    /// The space the request acts in, if it names one.
    pub fn named_space(&self) -> Option<&SpaceSelector> {
        match self {
            Self::Space { space, .. } => Some(space),
            Self::Checkout(_) => None,
        }
    }
}

impl ThreadNumber {
    /// Its full number, when it gives the space it is numbered in.
    pub fn full(&self) -> Option<&ItemNumber> {
        match self {
            Self::Full(number) => Some(number),
            Self::Own(_) => None,
        }
    }
}

impl Operation {
    /// Leave out what belongs to the caller's machine before the request
    /// goes to another: an approval's environment, where the owning
    /// machine pushes in its daemon's own, and a launch's directory and
    /// terminal, where that machine starts or checks the agent in a place
    /// of its own, or reads its modes, marking the launch as one another
    /// machine forwarded.
    pub fn leave_caller_machine(&mut self) {
        match self {
            Self::Approval(ApprovalMethod::DecideApproval { env, .. }) => *env = None,
            Self::Launch(
                LaunchMethod::Launch { launch } | LaunchMethod::CheckLaunch { launch },
            ) => {
                launch.directory = None;
                launch.env = None;
                launch.setup_env = None;
                launch.unplaceable = None;
                launch.forwarded.get_or_insert_default();
            }
            Self::Launch(
                LaunchMethod::LaunchModes { launch } | LaunchMethod::LabelledMode { launch },
            ) => {
                launch.directory = None;
                launch.forwarded.get_or_insert_default();
            }
            // Its worker runs in that machine's daemon's environment.
            Self::Operations(
                OperationMethod::StartMove { env, .. }
                | OperationMethod::StartOperation { env, .. },
            )
            | Self::Conversion(ConversionMethod::ConvertToWorkspace { env, .. }) => *env = None,
            // A process of this machine, which that machine has none of.
            Self::Removal(RemovalMethod::StopProcesses { requester, .. }) => *requester = None,
            _ => {}
        }
    }

    /// Whether the request decides or passes on an approval request it
    /// names by number alone, which another request may take while it
    /// waits.
    pub fn names_unpinned_item(&self) -> bool {
        match self {
            Self::Approval(
                ApprovalMethod::DecideApproval { shown, .. }
                | ApprovalMethod::EscalateApproval { shown, .. },
            ) => shown.is_none(),
            _ => false,
        }
    }

    /// Name the approval request a request decides or passes on as
    /// `approvals` holds it, where the caller named it by number alone, so
    /// that a later request reusing its number, as while its machine is
    /// offline, is refused instead.
    pub fn pin_item(&mut self, approvals: &[ApprovalRequest]) {
        let approval = |number: &ItemNumber| {
            (approvals.iter().find(|a| &a.number == number)).map(|a| Box::new(a.clone()))
        };
        match self {
            Self::Approval(
                ApprovalMethod::DecideApproval { number, shown, .. }
                | ApprovalMethod::EscalateApproval { number, shown, .. },
            ) if shown.is_none() => *shown = approval(number),
            _ => {}
        }
    }

    /// Whether the request can act on a space another joined machine runs:
    /// it forwards, and attaches no file by its path here, which that
    /// machine cannot read.
    pub fn reaches_other_machines(&self) -> bool {
        let attaches = serde_json::to_value(self).is_ok_and(|method| attaches_files(&method));
        // A turn's in-chat alert stays with the caller's execution, and the
        // daemon sends it to a space of another machine once the turn ends.
        let through_daemon = matches!(self, Self::Alert(AlertMethod::AlertInChat { .. }));
        (self.forwarding() != Forwarding::Never || through_daemon) && !attaches
    }

    /// Where the request runs when the space it acts on is another
    /// machine's, decided for every request by what it needs.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::Daemon(method) => method.forwarding(),
            Self::Repository(method) => method.forwarding(),
            Self::Workspace(method) => method.forwarding(),
            Self::Space(method) => method.forwarding(),
            Self::Place(method) => method.forwarding(),
            Self::Permission(method) => method.forwarding(),
            Self::Config(method) => method.forwarding(),
            Self::Diff(method) => method.forwarding(),
            Self::Merging(method) => method.forwarding(),
            Self::Removal(method) => method.forwarding(),
            Self::Recovery(method) => method.forwarding(),
            Self::Execution(method) => method.forwarding(),
            Self::Assignment(method) => method.forwarding(),
            Self::Agent(method) => method.forwarding(),
            Self::Chat(method) => method.forwarding(),
            Self::Conversion(method) => method.forwarding(),
            Self::Launch(method) => method.forwarding(),
            Self::Queue(method) => method.forwarding(),
            Self::Hold(method) => method.forwarding(),
            Self::Demo(method) => method.forwarding(),
            Self::File(method) => method.forwarding(),
            Self::Item(method) => method.forwarding(),
            Self::Ci(method) => method.forwarding(),
            Self::Branch(method) => method.forwarding(),
            Self::Alert(method) => method.forwarding(),
            Self::Operations(method) => method.forwarding(),
            Self::Thread(method) => method.forwarding(),
            Self::Change(method) => method.forwarding(),
            Self::Review(method) => method.forwarding(),
            Self::Approval(method) => method.forwarding(),
            Self::Port(method) => method.forwarding(),
            Self::Secret(method) => method.forwarding(),
            Self::Resource(method) => method.forwarding(),
            Self::Simulator(method) => method.forwarding(),
            Self::Container(method) => method.forwarding(),
            Self::Subscription(method) => method.forwarding(),
            Self::Trigger(method) => method.forwarding(),
            Self::Hub(method) => method.forwarding(),
            Self::App(method) => method.forwarding(),
        }
    }

    /// What the request acts on, which decides the machine that answers
    /// it; none for one that acts on no space, on several, or on one it
    /// creates.
    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::Daemon(method) => method.target(),
            Self::Repository(method) => method.target(),
            Self::Workspace(method) => method.target(),
            Self::Space(method) => method.target(),
            Self::Place(method) => method.target(),
            Self::Permission(method) => method.target(),
            Self::Config(method) => method.target(),
            Self::Diff(method) => method.target(),
            Self::Merging(method) => method.target(),
            Self::Removal(method) => method.target(),
            Self::Recovery(method) => method.target(),
            Self::Execution(method) => method.target(),
            Self::Assignment(method) => method.target(),
            Self::Agent(method) => method.target(),
            Self::Chat(method) => method.target(),
            Self::Conversion(method) => method.target(),
            Self::Launch(method) => method.target(),
            Self::Queue(method) => method.target(),
            Self::Hold(method) => method.target(),
            Self::Demo(method) => method.target(),
            Self::File(method) => method.target(),
            Self::Item(method) => method.target(),
            Self::Ci(method) => method.target(),
            Self::Branch(method) => method.target(),
            Self::Alert(method) => method.target(),
            Self::Operations(method) => method.target(),
            Self::Thread(method) => method.target(),
            Self::Change(method) => method.target(),
            Self::Review(method) => method.target(),
            Self::Approval(method) => method.target(),
            Self::Port(method) => method.target(),
            Self::Secret(method) => method.target(),
            Self::Resource(method) => method.target(),
            Self::Simulator(method) => method.target(),
            Self::Container(method) => method.target(),
            Self::Subscription(method) => method.target(),
            Self::Trigger(method) => method.target(),
            Self::Hub(method) => method.target(),
            Self::App(method) => method.target(),
        }
    }

    /// What of a removed space the request acts on, which the machine that
    /// keeps it answers when no machine has the space: its files, or its
    /// record where the request takes a removed space as well as a live
    /// one; none for one that acts on live spaces alone.
    pub fn kept(&self) -> Option<Keeps> {
        match self.target() {
            Some(Target::Files(_)) => Some(Keeps::Files),
            Some(Target::Removable(_)) => Some(Keeps::Record),
            Some(Target::Space(_) | Target::Item(_) | Target::Acting(_)) => None,
            None => match self {
                Self::Operations(method) => method.kept(),
                _ => None,
            },
        }
    }
}

/// Whether the request `method`, as JSON, attaches files by their path on
/// this machine, anywhere in it, as a message's do and an issue's or
/// comment's within its content.
pub fn attaches_files(method: &serde_json::Value) -> bool {
    match method {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            let files =
                key == "attachments" && value.as_array().is_some_and(|files| !files.is_empty());
            files || attaches_files(value)
        }),
        serde_json::Value::Array(items) => items.iter().any(attaches_files),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
