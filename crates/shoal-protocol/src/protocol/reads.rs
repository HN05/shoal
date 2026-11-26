//! Requests that only read state, which the daemon cancels when their client
//! hangs up. Cancelling one that changes state could leave it half done, so
//! those finish instead, including reads that first finish or record work.
use super::Operation;

impl Operation {
    pub fn reads_only(&self) -> bool {
        match self {
            Self::Daemon(method) => method.reads_only(),
            Self::Repository(method) => method.reads_only(),
            Self::Workspace(method) => method.reads_only(),
            Self::Space(method) => method.reads_only(),
            Self::Place(method) => method.reads_only(),
            Self::Permission(method) => method.reads_only(),
            Self::Config(method) => method.reads_only(),
            Self::Diff(method) => method.reads_only(),
            Self::Merging(method) => method.reads_only(),
            Self::Removal(method) => method.reads_only(),
            Self::Recovery(method) => method.reads_only(),
            Self::Execution(method) => method.reads_only(),
            Self::Assignment(method) => method.reads_only(),
            Self::Agent(method) => method.reads_only(),
            Self::Chat(method) => method.reads_only(),
            Self::Conversion(method) => method.reads_only(),
            Self::Launch(method) => method.reads_only(),
            Self::Queue(method) => method.reads_only(),
            Self::Hold(method) => method.reads_only(),
            Self::Demo(method) => method.reads_only(),
            Self::File(method) => method.reads_only(),
            Self::Item(method) => method.reads_only(),
            Self::Ci(method) => method.reads_only(),
            Self::Branch(method) => method.reads_only(),
            Self::Alert(method) => method.reads_only(),
            Self::Operations(method) => method.reads_only(),
            Self::Thread(method) => method.reads_only(),
            Self::Change(method) => method.reads_only(),
            Self::Review(method) => method.reads_only(),
            Self::Approval(method) => method.reads_only(),
            Self::Port(method) => method.reads_only(),
            Self::Secret(method) => method.reads_only(),
            Self::Resource(method) => method.reads_only(),
            Self::Simulator(method) => method.reads_only(),
            Self::Container(method) => method.reads_only(),
            Self::Subscription(method) => method.reads_only(),
            Self::Trigger(method) => method.reads_only(),
            Self::Hub(method) => method.reads_only(),
            Self::App(method) => method.reads_only(),
            Self::Machine(method) => method.reads_only(),
        }
    }
}

#[cfg(test)]
mod tests;
