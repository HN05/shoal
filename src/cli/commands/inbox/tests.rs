use super::{Inbox, Item};
use crate::{cli::output::Palette, model::CheckoutSync};

#[test]
fn a_lone_sync_failure_names_its_repository() {
    let inbox = Inbox {
        items: vec![Item::SyncFailed {
            sync: CheckoutSync {
                repository_id: "r1".into(),
                synced_at: None,
                failing_since: Some(1),
                error: Some("could not refresh main\nmore".into()),
            },
        }],
        workspaces: Vec::new(),
        chats: Vec::new(),
    };
    let repositories = [("r1".to_owned(), "app".to_owned())];
    let rows = inbox.rows(&repositories, Palette::stdout(true), None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("app"), "{rows:?}");
    assert!(rows[0].contains("could not refresh main"), "{rows:?}");
    assert!(!rows[0].contains("more"), "{rows:?}");
}
