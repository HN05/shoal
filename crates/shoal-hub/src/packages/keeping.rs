//! The hub's side of packages: it takes a machine's package of a space it
//! lists under that machine whole, in its state directory, before it
//! replaces the one it kept, refuses packages while its disk is below the
//! overload warning threshold, and deletes a package once the directory
//! drops its space on that machine.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Transaction, params};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_schedule::placement::snapshot::Request;
use tokio::sync::OwnedRwLockReadGuard;

use shoal_core::fsutil::off_runtime;
use shoal_daemon_base::notifications::About;
use shoal_protocol::alerts::AlertKind;

use crate::{
    Hub,
    daemon::Daemon,
    directory, log,
    streams::{Incoming, Outgoing, Streams},
    time::UnixTime,
};

/// The extension of a kept package's file, named by its space's ID.
const EXTENSION: &str = "shoal";

/// What the hub keeps of its packages while it runs.
#[derive(Default)]
pub(crate) struct Kept {
    /// Held while a package's file takes its place or goes, so a sweep never
    /// deletes one whose record is being written.
    files: tokio::sync::Mutex<()>,
    /// Whether the user heard that the hub refuses packages for its disk,
    /// which they hear again once it took one meanwhile.
    refusal_alerted: AtomicBool,
}

/// A package `member` sends of `space`, which it built at `built_at` by its
/// own clock, and which began to arrive at `arrived_at` by the hub's, with
/// what continues the space from it elsewhere.
pub(crate) struct Sent {
    pub member: MemberId,
    pub space: SpaceId,
    pub built_at: UnixTime,
    pub arrived_at: UnixTime,
    pub continuation: Continuation,
}

/// What continues a space from its package elsewhere: the start its agent
/// would make, which places it, and for a chat in a workspace's worktree,
/// that workspace, which it follows.
#[derive(Debug, Clone, Default)]
pub(crate) struct Continuation {
    pub start: Option<Request>,
    pub within: Option<SpaceId>,
}

impl Hub {
    /// Take the package `sent` that its machine writes up the duplex stream
    /// `stream` of `streams`, ending this side of it once the package is
    /// kept whole, or with why not; refused at once while the hub's disk is
    /// low or the directory lists the space under another machine or none.
    pub(crate) async fn keep_package(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        sent: Sent,
        streams: (&Streams, u64),
    ) -> Result<()> {
        let (out, up) = self.accept_package(daemon, &sent, streams).await?;
        let hub = self.clone();
        tokio::spawn(async move {
            let (kept, operation) = hub.take_package(sent, up).await;
            out.end(kept.err().map(|error| format!("{error:#}"))).await;
            drop(operation);
        });
        Ok(())
    }

    /// This side of the duplex stream `stream` of `streams` that the
    /// package `sent` comes up, and what comes up it; refused at once
    /// while the hub's disk is low or the directory lists the space under
    /// another machine or none.
    pub(super) async fn accept_package(
        &self,
        daemon: &dyn Daemon,
        sent: &Sent,
        (streams, stream): (&Streams, u64),
    ) -> Result<(Outgoing, Incoming)> {
        self.ensure_package_room(daemon).await?;
        let (member, space) = (sent.member.clone(), sent.space.clone());
        let listed = (self.store).read(move |tx| lists_under(tx, &space, &member));
        ensure!(
            listed.await?,
            "the hub lists space {} under another machine or none",
            sent.space
        );
        let (out, up) = streams.answering(stream, true);
        Ok((out, up.context("a package comes up a duplex stream")?))
    }

    /// Read the package `sent` from `up` to its end and keep it, and how
    /// that went, with the daemon operation it was kept in, which the
    /// caller holds until it answered, so an update never exits between
    /// the two; none once the daemon is exiting, which keeps nothing.
    pub(super) async fn take_package(
        &self,
        sent: Sent,
        up: Incoming,
    ) -> (Result<()>, Option<OwnedRwLockReadGuard<()>>) {
        let space = sent.space.clone();
        let received = self.receive_package(up).await;
        let operation = self.gates.operation_unless_exiting().await;
        let kept = match (received, &operation) {
            (Ok((incoming, size)), Some(_)) => self.place_package(sent, size, incoming).await,
            (Ok(_), None) => Err(anyhow::anyhow!(
                "the hub's daemon is restarting; send it again once it is back"
            )),
            (Err(error), _) => Err(error),
        };
        if let Err(error) = &kept {
            log!("hub: could not keep the package of space {space}: {error:#}");
        }
        (kept, operation)
    }

    /// The package its machine writes up `up`, read to its end into a new
    /// file beside the kept ones, with its size.
    async fn receive_package(&self, mut up: Incoming) -> Result<(tempfile::NamedTempFile, u64)> {
        let directory = self.package_directory();
        let incoming = off_runtime::run(move || {
            std::fs::create_dir_all(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
            (tempfile::Builder::new()
                .prefix(".incoming-")
                .tempfile_in(&directory))
            .with_context(|| format!("create a file in {}", directory.display()))
        })
        .await?;
        let size = up.write_to(incoming.reopen()?).await?;
        Ok((incoming, size))
    }

    /// Move `incoming`, which holds the package `sent` of `size` bytes, to
    /// its place, then record it while the directory still lists its space
    /// under its machine. A file that cannot take its place leaves the one
    /// before and its record as they were; one that cannot be recorded, as
    /// once its space ended, goes, and its machine sends the space again.
    async fn place_package(
        &self,
        sent: Sent,
        size: u64,
        incoming: tempfile::NamedTempFile,
    ) -> Result<()> {
        let path = self.package_path(&sent.space);
        let _files = self.packages.files.lock().await;
        let Sent {
            member,
            space,
            built_at,
            arrived_at,
            continuation,
        } = sent;
        (incoming.persist(&path))
            .map_err(|error| error.error)
            .with_context(|| format!("write {}", path.display()))?;
        let recorded = (self.store)
            .write(move |tx| {
                record(tx, (&member, &space), (built_at, size), &continuation)?;
                super::sleeps::caught_up(tx, &member, &space, arrived_at)
            })
            .await;
        if recorded.is_err() {
            let _ = off_runtime::run(move || Ok(std::fs::remove_file(path)?)).await;
        }
        recorded
    }

    /// Refuse a package while the disk of the hub's state directory has less
    /// free space than the overload warning threshold, alerting the user
    /// the first time since the hub last took one.
    async fn ensure_package_room(&self, daemon: &dyn Daemon) -> Result<()> {
        let threshold = (self.config.current().machine.overload.warning).disk_free_bytes();
        let state = self.paths.state.clone();
        let free = tokio::task::spawn_blocking(move || {
            shoal_core::fsutil::disk_space::available_bytes(&state)
        })
        .await??;
        let alerted = &self.packages.refusal_alerted;
        if free >= threshold {
            alerted.store(false, Ordering::Relaxed);
            return Ok(());
        }
        let message = format!(
            "The hub keeps no new packages of intermittent machines' spaces while its disk has {:.1} GiB free, below the overload warning threshold of {} GiB; once space is free, the machines send the spaces that changed",
            free as f64 / f64::from(1 << 30),
            threshold >> 30,
        );
        if !alerted.swap(true, Ordering::Relaxed) {
            let about = About::named(None);
            (self.notify(daemon, about, AlertKind::HubPackageRefused, message.clone())).await;
        }
        bail!("{message}")
    }

    /// Delete the files of the packages whose records went, as with their
    /// spaces.
    pub(crate) async fn delete_ended_packages(&self) {
        let directory = self.package_directory();
        let _files = self.packages.files.lock().await;
        let deleted = async {
            let kept = self.store.read(kept_spaces).await?;
            off_runtime::run(move || delete_files_without(&directory, &kept)).await
        };
        if let Err(error) = deleted.await {
            log!("hub: could not delete the packages of ended spaces: {error:#}");
        }
    }

    /// Where the hub keeps packages.
    fn package_directory(&self) -> PathBuf {
        self.paths.state.join("hub").join("packages")
    }

    /// The file of the package of the space `id`.
    pub(crate) fn package_path(&self, id: &SpaceId) -> PathBuf {
        (self.package_directory()).join(format!("{id}.{EXTENSION}"))
    }
}

/// Whether the directory lists the space `id` under `member`.
fn lists_under(tx: &Transaction<'_>, id: &SpaceId, member: &MemberId) -> Result<bool> {
    Ok(directory::owner(tx, id)?.as_ref() == Some(member))
}

/// Record `member`'s package of the space `id`, built at `built_at`, of
/// `size` bytes, with what continues the space from it, in place of the
/// one before, while the directory lists the space under `member`.
fn record(
    tx: &Transaction<'_>,
    (member, id): (&MemberId, &SpaceId),
    (built_at, size): (UnixTime, u64),
    continuation: &Continuation,
) -> Result<()> {
    ensure!(
        lists_under(tx, id, member)?,
        "space {id} ended on this machine while its package came"
    );
    let start = (continuation.start.as_ref())
        .map(serde_json::to_string)
        .transpose()?;
    tx.execute(
        "INSERT INTO hub_packages(space_id, member_id, built_at, size, kept_at, start, within)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(space_id) DO UPDATE SET member_id=excluded.member_id,
            built_at=excluded.built_at, size=excluded.size, kept_at=excluded.kept_at,
            start=excluded.start, within=excluded.within",
        params![
            id,
            member,
            built_at,
            size as i64,
            UnixTime::now(),
            start,
            continuation.within,
        ],
    )?;
    Ok(())
}

/// Forget every package `member` sent, as once the hub revoked it.
pub(crate) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_packages WHERE member_id=?1", [member])?;
    Ok(())
}

/// The spaces the hub keeps a package of.
fn kept_spaces(tx: &Transaction<'_>) -> Result<HashSet<String>> {
    let mut statement = tx.prepare("SELECT space_id FROM hub_packages")?;
    let ids = statement.query_map([], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

/// Delete the package files in `directory` whose spaces are not among
/// `kept`, leaving those still arriving.
fn delete_files_without(directory: &Path, kept: &HashSet<String>) -> Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        let space = (path
            .extension()
            .is_some_and(|extension| extension == EXTENSION))
        .then(|| path.file_stem().and_then(|stem| stem.to_str()))
        .flatten();
        if let Some(space) = space
            && !kept.contains(space)
        {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
