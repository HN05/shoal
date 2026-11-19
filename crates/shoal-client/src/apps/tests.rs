//! What an app keeps in the app store of a daemon on an isolated state
//! directory, as the user.
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::apps::{
    AppName,
    store::{AppScope, Write},
};

use crate::test_daemon::Connect as _;

fn quay() -> AppName {
    "quay".parse().unwrap()
}

fn write(key: &str, replaces: Option<shoal_protocol::apps::store::Revision>) -> Write {
    Write {
        key: key.parse().unwrap(),
        content_type: "image/png".parse().unwrap(),
        replaces,
    }
}

#[tokio::test]
async fn an_entry_larger_than_a_frame_is_written_and_read_back_whole() {
    let daemon = TestDaemon::start().await;
    let connector = daemon.connector();
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let written = super::write(
        &connector,
        quay(),
        AppScope::Shared,
        write("exports/a.png", None),
        &bytes,
    )
    .await
    .unwrap();
    assert_eq!(written.size, bytes.len() as u64);
    let key = "exports/a.png".parse().unwrap();
    let read = super::read(&connector, quay(), AppScope::Shared, key).await;
    let read = read.unwrap().unwrap();
    assert_eq!(read.entry, written);
    assert_eq!(read.bytes, bytes);
    // This machine's own entries hold none of the shared ones.
    let key = "exports/a.png".parse().unwrap();
    let own = super::read(&connector, quay(), AppScope::Device, key).await;
    assert_eq!(own.unwrap(), None);
}

#[tokio::test]
async fn a_write_from_a_stale_revision_is_refused_as_stale() {
    let daemon = TestDaemon::start().await;
    let connector = daemon.connector();
    let first = super::write(
        &connector,
        quay(),
        AppScope::Shared,
        write("layout/main", None),
        b"{}",
    )
    .await
    .unwrap();
    let replacing = write("layout/main", Some(first.revision));
    super::write(
        &connector,
        quay(),
        AppScope::Shared,
        replacing.clone(),
        b"[]",
    )
    .await
    .unwrap();
    let stale = super::write(&connector, quay(), AppScope::Shared, replacing, b"[1]").await;
    assert!(super::stale(&stale.unwrap_err()));
}
