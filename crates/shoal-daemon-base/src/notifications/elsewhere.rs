//! The user's alerts the other joined machines keep, as the hub last sent
//! them: this machine keeps them in memory, never in its store, and keeps
//! them while its link to the hub is down.
use std::collections::BTreeMap;

use shoal_ids::member_ids::MemberId;
use shoal_protocol::{alerts::Alert, hub::alerts::AlertCopies};

/// Each other machine's alerts to the user, by number.
#[derive(Default)]
pub(super) struct Elsewhere(BTreeMap<MemberId, BTreeMap<i64, Alert>>);

impl Elsewhere {
    /// Take what the hub sent.
    pub(super) fn take(&mut self, copies: AlertCopies) {
        match copies {
            AlertCopies::Whole(alerts) => {
                self.0.clear();
                for alert in alerts {
                    let Some(machine) = alert.machine.clone() else {
                        continue;
                    };
                    self.0
                        .entry(machine.id)
                        .or_default()
                        .insert(alert.id, alert);
                }
            }
            AlertCopies::Changed {
                machine,
                changed,
                removed,
            } => {
                let kept = self.0.entry(machine).or_default();
                for id in removed {
                    kept.remove(&id);
                }
                kept.extend(changed.into_iter().map(|alert| (alert.id, alert)));
            }
            AlertCopies::Dropped { machine } => {
                self.0.remove(&machine);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    /// Every other machine's alert to the user.
    pub(super) fn alerts(&self) -> impl Iterator<Item = &Alert> {
        self.0.values().flat_map(BTreeMap::values)
    }

    /// The highest number another machine's alert has.
    pub(super) fn highest(&self) -> Option<i64> {
        self.0
            .values()
            .filter_map(|kept| kept.keys().last())
            .max()
            .copied()
    }
}
