//! The machine column of lists that show other joined machines' spaces.
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::directory::HubView;

use super::Cell;
use crate::{cli::output::Style, time::UnixTime};

/// The machines rows name, from the hub's view of the other machines'
/// spaces: this one and the others.
pub struct Machines<'a> {
    view: &'a HubView,
    now: UnixTime,
}

impl<'a> Machines<'a> {
    pub fn new(view: &'a HubView) -> Self {
        Self {
            view,
            now: UnixTime::now(),
        }
    }

    /// This machine's cell.
    pub fn here(&self) -> Cell {
        self.cell(&self.view.machine)
    }

    /// The cell of the machine `id`: its name, with when the hub last saw
    /// it while it is not connected.
    pub fn cell(&self, id: &MemberId) -> Cell {
        let Some(machine) = self.view.directory.machine(id) else {
            return (id.to_string(), Some(Style::Muted));
        };
        let name = &machine.name;
        match (machine.connected, machine.last_seen) {
            (true, _) => (name.clone(), None),
            (false, Some(seen)) => {
                let seen = seen.local_ago(self.now);
                (format!("{name}, seen {seen}"), Some(Style::Warning))
            }
            (false, None) => (format!("{name}, never seen"), Some(Style::Warning)),
        }
    }
}
