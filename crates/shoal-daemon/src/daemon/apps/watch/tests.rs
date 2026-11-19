use std::collections::BTreeMap;

use shoal_protocol::apps::store::{Change, Entry, EntryKey, Revision};

use super::changed;
use crate::time::UnixTime;

fn entries(revisions: &[(&str, u64)]) -> BTreeMap<EntryKey, Entry> {
    (revisions.iter())
        .map(|(key, revision)| {
            let entry = Entry {
                key: key.parse().unwrap(),
                content_type: "application/json".parse().unwrap(),
                revision: Revision(*revision),
                size: 2,
                updated_at: UnixTime::now(),
            };
            (entry.key.clone(), entry)
        })
        .collect()
}

#[test]
fn a_watch_reports_each_entry_written_since_then_each_one_deleted() {
    let before = entries(&[("a", 1), ("b", 2), ("c", 3)]);
    let after = entries(&[("a", 1), ("b", 5), ("d", 6)]);
    let changes = changed(&before, &after);
    let keys: Vec<String> = (changes.iter())
        .map(|change| match change {
            Change::Written(entry) => format!("written {} at {}", entry.key, entry.revision),
            Change::Deleted(key) => format!("deleted {key}"),
        })
        .collect();
    assert_eq!(keys, ["written b at 5", "written d at 6", "deleted c"]);
}
