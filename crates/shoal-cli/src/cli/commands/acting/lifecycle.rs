use crate::cli::{
    args::lifecycle::{LifecycleCommand, StopArgs},
    commands::{spaces, stop, transfer, turns},
    ui::Acting,
};
use shoal_protocol::processes::Selection;

pub(super) fn acting(command: &LifecycleCommand) -> Option<Acting> {
    Some(match command {
        LifecycleCommand::Stop(StopArgs {
            interrupt: true, ..
        }) => turns::typing(),
        LifecycleCommand::Stop(StopArgs {
            processes: true, ..
        }) => stop::stopping_processes(Selection::Processes),
        LifecycleCommand::Stop(StopArgs {
            leftovers: true, ..
        }) => stop::stopping_processes(Selection::Leftovers),
        LifecycleCommand::Stop(_) => stop::stopping(),
        LifecycleCommand::Rm(_) => spaces::removing(),
        LifecycleCommand::Move(_) => transfer::moving(),
        // An absorption and cleanup pick no space.
        LifecycleCommand::Absorb(_) | LifecycleCommand::Cleanup { .. } => return None,
    })
}
