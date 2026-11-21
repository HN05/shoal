//! Taking a space over from the package the hub keeps of it while its
//! machine is offline: the hub says what it keeps of the space and writes
//! the package to the machine that takes it over, and records the takeover
//! once that machine claims the space, for the space's own machine to
//! retire its copy once it returns.
use std::{
    collections::HashSet,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_core::fsutil::off_runtime;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    link::{Call, Reply},
    packages::KeptPackage,
};

use crate::{
    Hub, answers::unexpected, daemon::Daemon, directory, membership, streams::Streams,
    time::UnixTime,
};

impl Hub {
    /// The package the hub keeps of the space `id`, whose machine is
    /// offline, as this machine would take the space over from it; none
    /// without one, while that machine is connected, and on a machine in
    /// no hub.
    pub async fn kept_package(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<KeptPackage>> {
        if self.is_hub(daemon) {
            return self.local_kept_package(id).await;
        }
        if self.store.read(membership::load).await?.is_none() {
            return Ok(None);
        }
        match self.through_hub(Call::Kept { space: id.clone() }).await? {
            Reply::Kept(kept) => Ok(kept.map(|kept| *kept)),
            reply => Err(unexpected(reply)),
        }
    }

    /// Write the package the hub keeps of the space `id` to the new file
    /// `file`, while its machine stays offline.
    pub async fn fetch_package(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        file: &Path,
    ) -> Result<()> {
        if self.is_hub(daemon) {
            let kept = self.offline_package_file(id).await?;
            let file = file.to_owned();
            return off_runtime::run(move || Ok(std::fs::copy(kept, file).map(drop)?)).await;
        }
        let calls = self.hub_calls().await?;
        let fetch = |stream| Call::Fetch {
            space: id.clone(),
            stream,
        };
        let mut incoming = calls.stream(fetch).await?;
        let path = file.to_owned();
        let output = off_runtime::run(move || {
            (std::fs::File::create_new(&path)).with_context(|| format!("create {}", path.display()))
        })
        .await?;
        incoming.write_to(output).await.map(drop)
    }

    /// As the hub: what it keeps of the space `id`, whose machine is
    /// offline, with the space as its package records it.
    pub(crate) async fn local_kept_package(&self, id: &SpaceId) -> Result<Option<KeptPackage>> {
        let Some((_, built_at)) = self.offline_package(id).await? else {
            return Ok(None);
        };
        let path = self.package_path(id);
        let manifest = off_runtime::run(move || {
            let file =
                std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
            shoal_core::transfer::read_manifest(&mut BufReader::new(file))
        })
        .await?;
        Ok(Some(KeptPackage {
            record: manifest.space.record,
            built_at,
        }))
    }

    /// As the hub: write the package it keeps of the space `id`, whose
    /// machine is offline, on the stream `stream` of `streams`, in the
    /// background.
    pub(crate) async fn serve_fetch(
        self: &Arc<Self>,
        id: &SpaceId,
        (streams, stream): (&Streams, u64),
    ) -> Result<()> {
        let path = self.offline_package_file(id).await?;
        let (mut out, _) = streams.answering(stream, false);
        tokio::spawn(async move {
            let written = out.write_file(&path).await;
            out.end(written.err().map(|error| format!("{error:#}")))
                .await;
        });
        Ok(())
    }

    /// The file of the package the hub keeps of the space `id`, refused
    /// without one or while its machine is connected.
    async fn offline_package_file(&self, id: &SpaceId) -> Result<PathBuf> {
        match self.offline_package(id).await? {
            Some(_) => Ok(self.package_path(id)),
            None => bail!(
                "the hub keeps no package of space {id} whose machine is offline; while its machine is connected, shoal move {id} --machine moves it"
            ),
        }
    }

    /// The machine the directory lists the space `id` under and when it
    /// built the package the hub keeps of it, while that machine is
    /// offline.
    async fn offline_package(&self, id: &SpaceId) -> Result<Option<(MemberId, UnixTime)>> {
        let connected = self.links.connected();
        let id = id.clone();
        let kept = self.store.read(move |tx| owners_package(tx, &id)).await?;
        Ok(kept.filter(|(member, _)| !connected.contains(member)))
    }
}

/// The machine the directory lists the space `id` under and when it built
/// the package the hub keeps of it, if it sent one.
fn owners_package(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<(MemberId, UnixTime)>> {
    let Some(owner) = directory::owner(tx, id)? else {
        return Ok(None);
    };
    Ok(tx
        .query_row(
            "SELECT built_at FROM hub_packages WHERE space_id=?1 AND member_id=?2",
            params![id, owner],
            |row| row.get(0),
        )
        .optional()?
        .map(|built_at| (owner, built_at)))
}

/// Record that `claimer`'s claim of the space `id` takes it over from the
/// package the hub keeps of it, refused unless the machine the directory
/// lists it under, which sent that package, is another than `claimer` and
/// not among the `connected` ones.
pub(crate) fn record_takeover(
    tx: &Transaction<'_>,
    id: &SpaceId,
    claimer: &MemberId,
    connected: &HashSet<MemberId>,
) -> Result<()> {
    let Some((owner, _)) = owners_package(tx, id)? else {
        bail!("the hub keeps no package of space {id} to take it over from");
    };
    ensure!(&owner != claimer, "space {id} is already this machine's");
    ensure!(
        !connected.contains(&owner),
        "the machine of space {id} is connected again, so the space stays there; shoal move {id} --machine moves it"
    );
    tx.execute(
        "INSERT INTO hub_takeovers(space_id, member_id, taken_at) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET member_id=excluded.member_id,
            taken_at=excluded.taken_at",
        params![id, owner, UnixTime::now()],
    )?;
    Ok(())
}

/// Forget the takeovers of spaces taken from `member`, as once the hub
/// revoked it.
pub(crate) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_takeovers WHERE member_id=?1", [member])?;
    Ok(())
}

#[cfg(test)]
mod tests;
