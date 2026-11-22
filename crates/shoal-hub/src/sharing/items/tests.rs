use std::collections::BTreeMap;

use shoal_protocol::hub::shared_config::FileName;

use shoal_protocol::hub::sharing;

use super::*;
use crate::shared_config::SharedFiles;

fn mode(name: &str) -> FileName {
    FileName::Mode(name.into())
}

fn copy(key: FileName, text: Option<&str>, revision: i64) -> Copy<SharedFiles> {
    sharing::Copy {
        key,
        value: text.map(Into::into),
        revision,
    }
}

fn edit(key: FileName, text: Option<&str>) -> Edit<SharedFiles> {
    sharing::Edit {
        key,
        value: text.map(Into::into),
    }
}

#[test]
fn a_machine_sends_what_differs_from_its_agreed_copies_and_new_items_once_it_knows_the_hub_s() {
    let on_disk = BTreeMap::from([
        (FileName::Config, "edited".to_owned()),
        (mode("same"), "same".to_owned()),
        (mode("new"), "new".to_owned()),
    ]);
    let agreed = BTreeMap::from([
        (FileName::Config, copy(FileName::Config, Some("agreed"), 1)),
        (mode("same"), copy(mode("same"), Some("same"), 2)),
        (mode("removed"), copy(mode("removed"), Some("gone"), 3)),
        (mode("absent"), copy(mode("absent"), None, 4)),
    ]);
    assert_eq!(
        edits::<SharedFiles>(&on_disk, &agreed, false),
        [
            edit(FileName::Config, Some("edited")),
            edit(mode("removed"), None),
        ]
    );
    assert_eq!(
        edits::<SharedFiles>(&on_disk, &agreed, true),
        [
            edit(FileName::Config, Some("edited")),
            edit(mode("new"), Some("new")),
            edit(mode("removed"), None),
        ]
    );
}

#[test]
fn a_copy_replaces_an_edit_only_where_the_hub_never_got_it() {
    let hubs = copy(FileName::Config, Some("hub's"), 5);
    let local = "local".to_owned();
    let agreed = copy(FileName::Config, Some("agreed"), 4);
    let cases = [
        // The machine agreed on this copy, or a later one.
        (
            Some(&local),
            Some(copy(FileName::Config, Some("x"), 5)),
            None,
            Taking::Stale,
        ),
        // The item holds the copy, or the edit the copy is.
        (
            Some(&"hub's".to_owned()),
            Some(agreed.clone()),
            None,
            Taking::Agree,
        ),
        (
            Some(&local),
            Some(agreed.clone()),
            Some(edit(FileName::Config, Some("hub's"))),
            Taking::Agree,
        ),
        // The item holds what the machine agreed on before.
        (
            Some(&"agreed".to_owned()),
            Some(agreed.clone()),
            None,
            Taking::Write { keep: false },
        ),
        // An edit since then, or an item the machine never agreed on.
        (
            Some(&local),
            Some(agreed.clone()),
            None,
            Taking::Write { keep: true },
        ),
        (Some(&local), None, None, Taking::Write { keep: true }),
        // A removed item has nothing to keep.
        (
            None,
            Some(agreed.clone()),
            None,
            Taking::Write { keep: false },
        ),
        (None, None, None, Taking::Write { keep: false }),
    ];
    for (on_disk, agreed, sent, expected) in cases {
        let taken = taking::<SharedFiles>(&hubs, on_disk, agreed.as_ref(), sent.as_ref());
        assert_eq!(
            taken, expected,
            "{on_disk:?} agreed {agreed:?} sent {sent:?}"
        );
    }
}
