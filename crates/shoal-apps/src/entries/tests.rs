use shoal_protocol::apps::{
    AppName,
    store::{Entry, Revision, Write},
};

use super::Holder;
use crate::{Apps, refusal::Reason, stand_in::Fixture};

fn quay() -> AppName {
    "quay".parse().unwrap()
}

fn write(key: &str, replaces: Option<Revision>) -> Write {
    Write {
        key: key.parse().unwrap(),
        content_type: "application/json".parse().unwrap(),
        replaces,
    }
}

async fn written(apps: &Apps, holder: Holder, write: Write, bytes: &[u8]) -> Entry {
    (apps.write_entry(holder, quay(), write, bytes.to_vec()))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_write_from_a_stale_revision_is_refused_and_the_entry_kept() {
    let fixture = Fixture::new().await;
    let apps = &fixture.apps;
    let first = written(apps, Holder::Shared, write("layout/main", None), b"{}").await;
    let second = written(
        apps,
        Holder::Shared,
        write("layout/main", Some(first.revision)),
        b"[1]",
    )
    .await;
    assert!(second.revision > first.revision);
    for stale in [Some(first.revision), None] {
        let refused = (apps.write_entry(
            Holder::Shared,
            quay(),
            write("layout/main", stale),
            b"[2]".to_vec(),
        ))
        .await
        .unwrap_err();
        assert_eq!(Reason::of(&refused), Some(Reason::StaleRevision));
    }
    let key = "layout/main".parse().unwrap();
    let deleted = apps.delete_entry(Holder::Shared, quay(), key, first.revision);
    assert_eq!(
        Reason::of(&deleted.await.unwrap_err()),
        Some(Reason::StaleRevision)
    );
    let key = "layout/main".parse().unwrap();
    let kept = apps.read_entry(Holder::Shared, quay(), key).await;
    let kept = kept.unwrap().unwrap();
    assert_eq!(kept.bytes, b"[1]");
    assert_eq!(kept.entry, second);
}

#[tokio::test]
async fn an_entry_deleted_and_written_again_takes_a_new_revision() {
    let fixture = Fixture::new().await;
    let apps = &fixture.apps;
    let first = written(apps, Holder::Own, write("a", None), b"1").await;
    let key = "a".parse().unwrap();
    (apps.delete_entry(Holder::Own, quay(), key, first.revision))
        .await
        .unwrap();
    let again = written(apps, Holder::Own, write("a", None), b"1").await;
    assert!(again.revision > first.revision);
}

#[tokio::test]
async fn shared_entries_and_this_machine_s_own_are_apart() {
    let fixture = Fixture::new().await;
    let apps = &fixture.apps;
    written(apps, Holder::Shared, write("layout/main", None), b"shared").await;
    written(apps, Holder::Own, write("layout/main", None), b"own").await;
    written(apps, Holder::Own, write("layout/side", None), b"own").await;
    written(apps, Holder::Own, write("exports/a", None), b"own").await;
    let prefix = "layout/".parse().unwrap();
    let own = apps.list_entries(Holder::Own, quay(), prefix).await;
    let keys: Vec<String> = (own.unwrap().iter())
        .map(|entry| entry.key.to_string())
        .collect();
    assert_eq!(keys, ["layout/main", "layout/side"]);
    let key = "layout/main".parse().unwrap();
    let shared = apps.read_entry(Holder::Shared, quay(), key).await;
    assert_eq!(shared.unwrap().unwrap().bytes, b"shared");
}

#[tokio::test]
async fn a_write_past_the_limits_is_refused_before_its_bytes_come() {
    let fixture = Fixture::new().await;
    let apps = &fixture.apps;
    let limits = "[machine.app_store]\nmax_entry_mib = 1\nmax_app_mib = 2\n";
    fixture.configure(limits);
    let mib = 1 << 20;
    let too_large = apps.check_write(Holder::Shared, quay(), write("big", None), mib + 1);
    assert!(too_large.await.is_err());
    written(
        apps,
        Holder::Shared,
        write("one", None),
        &vec![0; mib as usize],
    )
    .await;
    let first = written(
        apps,
        Holder::Own,
        write("two", None),
        &vec![0; mib as usize],
    )
    .await;
    // Every entry of the app counts, whoever holds it.
    let full = apps.check_write(Holder::Shared, quay(), write("three", None), 1);
    assert!(full.await.is_err());
    // Replacing an entry counts only what it adds.
    let replacing = write("two", Some(first.revision));
    let fits = apps.check_write(Holder::Own, quay(), replacing, mib);
    fits.await.unwrap();
    // Another app has room of its own.
    let other = "other".parse().unwrap();
    let elsewhere = apps.check_write(Holder::Shared, other, write("a", None), mib);
    elsewhere.await.unwrap();
}
