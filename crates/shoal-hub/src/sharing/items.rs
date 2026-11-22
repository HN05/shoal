//! A machine's shared items of a kind against the copies it agreed on with
//! the hub: which of them hold edits to send, and what each copy the hub
//! sends does to them.
use std::collections::BTreeSet;

use shoal_protocol::hub::sharing;

use super::{Agreed, Copy, Edit, Items, Kind};

/// The edits `items`, this machine's shared items, hold against the copies
/// it `agreed` on with the hub: each item that differs from its agreed
/// copy, and, once it `knows` the hub's copies, each item without one,
/// which the hub has none of.
pub(super) fn edits<S: Kind>(items: &Items<S>, agreed: &Agreed<S>, knows: bool) -> Vec<Edit<S>> {
    let keys: BTreeSet<&S::Key> = items.keys().chain(agreed.keys()).collect();
    (keys.into_iter())
        .filter_map(|key| {
            let value = items.get(key);
            let edited = match agreed.get(key) {
                Some(agreed) => agreed.value.as_ref() != value,
                None => knows && value.is_some(),
            };
            edited.then(|| sharing::Edit {
                key: key.clone(),
                value: value.cloned(),
            })
        })
        .collect()
}

/// What a copy from the hub does to a machine's item.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Taking {
    /// Nothing: the machine agreed on this copy or a later one.
    Stale,
    /// The machine agrees on it, which its item already holds, or which
    /// is the edit it sent.
    Agree,
    /// It replaces the item, whose value the kind may keep when `keep`,
    /// since it holds an edit the hub never got.
    Write { keep: bool },
}

/// Whether the hub's `copy` is one the machine `agreed` on, or older.
pub(crate) fn stale<S: Kind>(copy: &Copy<S>, agreed: Option<&Copy<S>>) -> bool {
    agreed.is_some_and(|agreed| agreed.revision >= copy.revision)
}

/// What the hub's `copy` does to the item that holds `item` here, given the
/// copy of it the machine `agreed` on before and the edit of it the
/// machine `sent` for this copy, if any.
pub(crate) fn taking<S: Kind>(
    copy: &Copy<S>,
    item: Option<&S::Value>,
    agreed: Option<&Copy<S>>,
    sent: Option<&Edit<S>>,
) -> Taking {
    if stale::<S>(copy, agreed) {
        return Taking::Stale;
    }
    // An item edited again since it was sent keeps that edit for the next
    // round to send.
    if item == copy.value.as_ref() || sent.is_some_and(|sent| sent.value == copy.value) {
        return Taking::Agree;
    }
    let edited = match agreed {
        Some(agreed) => agreed.value.as_ref() != item,
        None => item.is_some(),
    };
    Taking::Write {
        keep: edited && item.is_some(),
    }
}

#[cfg(test)]
mod tests;
