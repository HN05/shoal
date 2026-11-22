//! Item link commands: the request each sends for the space it acts on.
use crate::cli::{
    args::items::ItemsCommand,
    commands::{links, threads},
    ui::Acting,
};

/// What an item link command sends for the space it acts on.
pub(super) fn acting(command: &ItemsCommand) -> Option<Acting> {
    match command {
        // A thread is linked by its number, and a review watched by its ID.
        ItemsCommand::Link { items } | ItemsCommand::Unlink { items }
            if threads::selects_thread(items) =>
        {
            None
        }
        ItemsCommand::Watch { items, .. } if items.selects_review() => None,
        ItemsCommand::Link { items } => Some(links::linking(items.relation().ok()?)),
        ItemsCommand::Unlink { items } => Some(links::unlinking(items.relation().ok()?)),
        ItemsCommand::Watch { .. } => Some(links::watching()),
        ItemsCommand::Ready { undo: true, .. } => Some(links::clearing()),
        ItemsCommand::Ready { .. } => Some(links::marking()),
        // Items are viewed in the context of the space they name.
        ItemsCommand::View(_) => None,
    }
}
