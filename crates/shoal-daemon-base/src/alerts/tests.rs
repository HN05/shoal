use super::*;

use crate::notifications;

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, crate::store::Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, store)
}

/// A message to the user that attaches the copies at `paths`.
fn attaching(paths: &[&str]) -> NewAlert {
    NewAlert {
        attachments: paths.iter().map(PathBuf::from).collect(),
        ..NewAlert::shoal(AlertKind::Message, About::default(), "see".into())
    }
}

/// A file another machine keeps.
fn relayed() -> SentFile {
    SentFile {
        id: 4,
        space: SpaceId::from("away"),
        name: "plan.md".into(),
        title: None,
        copy: KeptCopy::Elsewhere {
            machine: "desk".into(),
        },
    }
}

#[tokio::test]
async fn an_alert_keeps_its_copies_in_order_and_its_relayed_files_after_them() {
    let (_temp, store) = store().await;
    let (paths, alerts) = store
        .write(|tx| {
            let alert = NewAlert {
                relayed: vec![relayed()],
                ..attaching(&["/files/b.png", "/files/a.png"])
            };
            let id = insert(tx, None, &alert, false, false)?;
            Ok((attachments(tx, id)?, select(tx, "", [])?))
        })
        .await
        .unwrap();
    assert_eq!(
        paths,
        [Path::new("/files/b.png"), Path::new("/files/a.png")]
    );
    // No space keeps a record of either copy here, so only the relayed
    // file is shown.
    let [alert] = alerts.as_slice() else {
        panic!("{alerts:?}");
    };
    assert_eq!(alert.files, [relayed()]);
}

#[tokio::test]
async fn an_alert_s_copies_follow_its_new_number_and_go_with_it() {
    let (_temp, store) = store().await;
    let (moved, left) = store
        .write(|tx| {
            let id = insert(tx, None, &attaching(&["/files/a.png"]), false, false)?;
            numbering::renumber(tx, id, 40)?;
            let moved = attachments(tx, 40)?;
            tx.execute("DELETE FROM alerts WHERE id=40", [])?;
            let left: i64 = tx.query_row("SELECT COUNT(*) FROM alert_attachments", [], |row| {
                row.get(0)
            })?;
            Ok((moved, left))
        })
        .await
        .unwrap();
    assert_eq!(moved, [Path::new("/files/a.png")]);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_notification_repeats_only_with_the_same_copies_in_the_same_order() {
    let (_temp, store) = store().await;
    let ids = store
        .write(|tx| {
            let first = notifications::insert(tx, &attaching(&["/a", "/b"]))?.id;
            let reordered = notifications::insert(tx, &attaching(&["/b", "/a"]))?.id;
            let again = notifications::insert(tx, &attaching(&["/a", "/b"]))?;
            Ok([first, reordered, again.id, i64::from(again.new)])
        })
        .await
        .unwrap();
    let [first, reordered, again, new] = ids;
    assert_ne!(reordered, first);
    assert_eq!((again, new), (first, 0));
}
