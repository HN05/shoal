//! The app store across the hub: the hub keeps the entries the machines
//! share, which every machine reaches through it, while each machine keeps
//! its own.
use shoal_client::{
    apps::{self, Watch, Watched},
    connection::Connector,
};
use shoal_protocol::apps::{
    AppName,
    store::{AppScope, Change, Entry, Revision, Write},
};

use super::{
    directory::{join, machine},
    following::app,
};

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

async fn written(app: &Connector, scope: AppScope, write: Write, bytes: &[u8]) -> Entry {
    apps::write(app, quay(), scope, write, bytes).await.unwrap()
}

async fn read(app: &Connector, scope: AppScope, key: &str) -> Option<Vec<u8>> {
    let read = apps::read(app, quay(), scope, key.parse().unwrap()).await;
    read.unwrap().map(|stored| stored.bytes)
}

/// Wait until `watch` reports `entry` written, past the changes before
/// it, which a watch reports as they come or together as the latest.
async fn sees(watch: &mut Watch<Connector>, entry: &Entry) {
    let seen = async {
        loop {
            match watch.next().await.unwrap() {
                Watched::Changed(Change::Written(written))
                    if written.revision >= entry.revision =>
                {
                    return written;
                }
                Watched::Changed(_) => {}
                watched => panic!("expected a change, not {watched:?}"),
            }
        }
    };
    let seen = tokio::time::timeout(std::time::Duration::from_secs(60), seen).await;
    assert_eq!(&seen.unwrap(), entry);
}

#[test]
fn joined_machines_see_each_others_shared_entries_change_and_refuse_stale_writes() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let (on_hub, on_laptop) = (app(&hub), app(&laptop));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    // What the laptop kept before it joined.
    runtime.block_on(async {
        written(
            &on_laptop,
            AppScope::Shared,
            write("layout/main", None),
            b"old",
        )
        .await;
        written(
            &on_laptop,
            AppScope::Device,
            write("layout/main", None),
            b"own",
        )
        .await;
    });
    join(&laptop, &hub, "laptop");
    runtime.block_on(async {
        // The hub's shared entries, not those the laptop held, and the
        // laptop's own entries still.
        assert_eq!(
            read(&on_laptop, AppScope::Shared, "layout/main").await,
            None
        );
        assert_eq!(
            read(&on_laptop, AppScope::Device, "layout/main").await,
            Some(b"own".to_vec())
        );
        let prefix = || "layout/".parse().unwrap();
        let mut on_hub_watch = Watch::open(on_hub.clone(), quay(), AppScope::Shared, prefix())
            .await
            .unwrap();
        let mut on_laptop_watch =
            Watch::open(on_laptop.clone(), quay(), AppScope::Shared, prefix())
                .await
                .unwrap();
        for watch in [&mut on_hub_watch, &mut on_laptop_watch] {
            let first = watch.next().await.unwrap();
            assert_eq!(
                first,
                Watched::Entries {
                    entries: Vec::new(),
                    resynced: false
                }
            );
        }
        let laptops = written(
            &on_laptop,
            AppScope::Shared,
            write("layout/main", None),
            b"1",
        )
        .await;
        sees(&mut on_hub_watch, &laptops).await;
        let replacing = write("layout/main", Some(laptops.revision));
        let hubs = written(&on_hub, AppScope::Shared, replacing.clone(), b"2").await;
        sees(&mut on_laptop_watch, &hubs).await;
        // The laptop writes from the revision it read before the hub's.
        let stale = apps::write(&on_laptop, quay(), AppScope::Shared, replacing, b"3").await;
        assert!(apps::stale(&stale.unwrap_err()));
        assert_eq!(
            read(&on_laptop, AppScope::Shared, "layout/main").await,
            Some(b"2".to_vec())
        );
        // Each machine's own entries stay its own.
        assert_eq!(read(&on_hub, AppScope::Device, "layout/main").await, None);
    });
}
