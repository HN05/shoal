//! The shared config across a hub's machines: `shared.toml` and the mode
//! and rule files beside it. Each machine, the hub's own included, looks
//! at its shared files every [`LOOK_EVERY`] and on each config reload, and
//! sends the hub each file it edited since it last agreed on that file with
//! the hub, once the files are valid; on each link it first reads the hub's
//! copies. The hub keeps the last edit of each file that reached it and
//! sends it to every other machine, which writes it. A copy that replaces an
//! edit the hub never got leaves that edit as a backup, with an alert.
use std::{collections::BTreeMap, convert::Infallible, sync::Arc, time::Duration};

use anyhow::{Result, anyhow, bail};
use shoal_core::config::Shared;
use shoal_daemon_base::notifications::About;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        PublicKey,
        link::{Call, Reply},
        shared_config::{Copy, Edit, FileName},
    },
};

use crate::{Hub, daemon::Daemon, log, membership, session::Calls};

mod agreed;
mod copies;
mod local;

/// How often a machine looks at its shared files for edits; a reload
/// looks at once.
const LOOK_EVERY: Duration = Duration::from_secs(10);

/// A machine's side of the shared config.
#[derive(Default)]
pub(super) struct Sharing {
    /// Held while the machine compares its shared files with the hub's
    /// copies and takes them, so a copy the hub sends waits for a round
    /// that sent an edit of its file.
    round: tokio::sync::Mutex<()>,
    /// The problem with the shared files the user last heard of, which
    /// keeps them from the hub.
    problem: std::sync::Mutex<Option<String>>,
}

/// Where a machine's edits go: over its link, or, on the hub's own
/// machine, to the hub as its own machine's.
enum To<'a> {
    Link(&'a Calls),
    Own(MemberId),
}

impl To<'_> {
    fn own(&self) -> bool {
        matches!(self, Self::Own(_))
    }
}

impl Hub {
    /// Share this machine's files over its link to the hub for as long as
    /// it stays open.
    pub(super) async fn keep_sharing(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        calls: &Calls,
    ) -> Infallible {
        self.keep_sharing_with(daemon, &To::Link(calls)).await
    }

    /// Share the hub's own machine's files while this daemon is the hub.
    pub(super) async fn keep_sharing_own(self: &Arc<Self>, daemon: &Arc<dyn Daemon>) {
        let mut reloaded = self.config.watch();
        loop {
            if self.is_hub(&**daemon) {
                match self.own_member().await {
                    Ok(own) => {
                        let own = To::Own(own);
                        tokio::select! {
                            never = self.keep_sharing_with(daemon, &own) => match never {},
                            () = self.hub_role_ended(&**daemon) => {}
                        }
                    }
                    Err(error) => {
                        log!("hub: could not share the shared config: {error:#}");
                        tokio::time::sleep(LOOK_EVERY).await;
                    }
                }
            } else if reloaded.changed().await.is_err() {
                return;
            }
            reloaded.borrow_and_update();
        }
    }

    /// Resolves once a reload ends this daemon's hub role.
    async fn hub_role_ended(&self, daemon: &dyn Daemon) {
        let mut reloaded = self.config.watch();
        while self.is_hub(daemon) {
            if reloaded.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Share this machine's files with `hub`: read its copies first, then
    /// send each edit as it is seen.
    async fn keep_sharing_with(self: &Arc<Self>, daemon: &Arc<dyn Daemon>, hub: &To<'_>) -> ! {
        let mut reloaded = self.config.watch();
        let mut knows = false;
        loop {
            match self.share_round(daemon, hub, knows).await {
                // Files the hub has none of go at once.
                Ok(()) if !knows => {
                    knows = true;
                    continue;
                }
                Ok(()) => {}
                Err(error) => log!("hub: could not share the shared config: {error:#}"),
            }
            tokio::select! {
                () = tokio::time::sleep(LOOK_EVERY) => {}
                changed = reloaded.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }
        }
    }

    /// Send `hub` this machine's edits, once its files are valid, and take
    /// the copies it answers with; until the machine `knows` the hub's
    /// copies, it reads them, sending only edits of files agreed on before.
    async fn share_round(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
        knows: bool,
    ) -> Result<()> {
        let _round = self.sharing.round.lock().await;
        let key = self.shared_hub_key(hub.own()).await?;
        let agreed = self.agreed_copies(&key).await?;
        let on_disk = self.shared_files().await?;
        let mut edits = local::edits(&on_disk, &agreed, knows);
        if !edits.is_empty() && !self.shared_files_valid(&**daemon).await {
            edits.clear();
        }
        if edits.is_empty() && knows {
            return Ok(());
        }
        let copies = self.share_with(daemon, hub, edits.clone()).await?;
        // Held so the daemon never exits between writing a copy and
        // recording that it agrees on it, as answering the hub's calls is.
        let Some(_operation) = self.gates.operation_unless_exiting().await else {
            bail!("the daemon is exiting");
        };
        self.take_copies(&**daemon, &key, copies, agreed, edits)
            .await
    }

    /// Send `hub` `edits`, returning its copy of every file.
    async fn share_with(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
        edits: Vec<Edit>,
    ) -> Result<Vec<Copy>> {
        match hub {
            To::Own(own) => self.take_shared_edits(daemon, own, edits).await,
            To::Link(calls) => match calls.call(Call::ShareFiles { edits }).await? {
                Reply::SharedFiles(copies) => Ok(copies),
                reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
            },
        }
    }

    /// The key of the hub, this daemon itself when `own`, which tells the
    /// copies agreed on with it from those of another hub.
    async fn shared_hub_key(&self, own: bool) -> Result<PublicKey> {
        if own {
            return Ok(self.hub_identity().await?.public.clone());
        }
        let joined = self.store.read(membership::load).await?;
        let joined = joined.ok_or_else(|| anyhow!("this machine joined no hub"))?;
        Ok(joined.hub_key)
    }

    /// Take the `copies` the hub sends this machine, or its own machine
    /// when `own`.
    pub(super) async fn take_sent_copies(
        &self,
        daemon: &dyn Daemon,
        own: bool,
        copies: Vec<Copy>,
    ) -> Result<()> {
        let _round = self.sharing.round.lock().await;
        let key = self.shared_hub_key(own).await?;
        let agreed = self.agreed_copies(&key).await?;
        self.take_copies(daemon, &key, copies, agreed, Vec::new())
            .await
    }

    /// Take the hub's `copies` into this machine's files, given the copies
    /// it `agreed` on with the hub whose key is `key` and the edits it
    /// `sent` for them, then load the config they make and tell the user
    /// of each edit a copy replaced.
    async fn take_copies(
        &self,
        daemon: &dyn Daemon,
        key: &PublicKey,
        copies: Vec<Copy>,
        agreed: BTreeMap<FileName, Copy>,
        sent: Vec<Edit>,
    ) -> Result<()> {
        let paths = self.paths.clone();
        let taken = move || local::take(&paths, copies, &agreed, &sent);
        let taken = tokio::task::spawn_blocking(taken).await??;
        let key = key.clone();
        let copies = taken.agreed;
        (self.store)
            .write(move |tx| agreed::record(tx, &key, &copies))
            .await?;
        if taken.wrote {
            self.reload_shared(daemon).await;
        }
        if !taken.kept.is_empty() {
            self.tell_kept(daemon, &taken.kept).await;
        }
        Ok(())
    }

    /// Load the config the hub's copies make, telling the user when it
    /// does not load, which leaves the previous settings in place.
    async fn reload_shared(&self, daemon: &dyn Daemon) {
        if let Err(error) = daemon.reload_config().await {
            let message = format!(
                "The shared config the hub sent does not load on this machine, which keeps its previous settings: {error:#}; fix it, then run shoal config reload"
            );
            log!("hub: {message}");
            (self.notify(
                daemon,
                About::named(None),
                AlertKind::EnvironmentProblem,
                message,
            ))
            .await;
        }
    }

    /// Tell the user that the hub's copies replaced edits it never got,
    /// which stay at the backups `kept`.
    async fn tell_kept(&self, daemon: &dyn Daemon, kept: &[std::path::PathBuf]) {
        let kept: Vec<String> = (kept.iter())
            .map(|path| path.display().to_string())
            .collect();
        let message = format!(
            "The hub's copy of the shared config replaced edits this machine had not sent it; they stay in {}",
            kept.join(", ")
        );
        (self.notify(
            daemon,
            About::named(None),
            AlertKind::SharedFileKept,
            message,
        ))
        .await;
    }

    /// Whether this machine's shared files load, telling the user once of
    /// each new problem that keeps them from the hub.
    async fn shared_files_valid(&self, daemon: &dyn Daemon) -> bool {
        let paths = self.paths.clone();
        let loaded = tokio::task::spawn_blocking(move || Shared::load(&paths)).await;
        let problem = match loaded {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => Some(format!("{error:#}")),
        };
        let told = std::mem::replace(&mut *self.sharing.problem.lock().unwrap(), problem.clone());
        let Some(problem) = problem else {
            return true;
        };
        if told.as_ref() != Some(&problem) {
            let message = format!(
                "The shared config is invalid, so this machine sends the hub none of its edits: {problem}; fix it, then run shoal config reload"
            );
            log!("hub: {message}");
            (self.notify(
                daemon,
                About::named(None),
                AlertKind::EnvironmentProblem,
                message,
            ))
            .await;
        }
        false
    }

    /// This machine's shared files.
    async fn shared_files(&self) -> Result<BTreeMap<FileName, String>> {
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || Shared::files(&paths)).await?
    }

    /// The copies this machine agreed on with the hub whose key is `key`.
    async fn agreed_copies(&self, key: &PublicKey) -> Result<BTreeMap<FileName, Copy>> {
        let key = key.clone();
        self.store.read(move |tx| agreed::load(tx, &key)).await
    }

    /// As the hub: keep the `edits` of the machine `from` that change its
    /// copies, send what they changed to every other machine, and return
    /// its copy of every file. From then on, `from` receives the copies
    /// others change.
    pub(super) async fn take_shared_edits(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        from: &MemberId,
        edits: Vec<Edit>,
    ) -> Result<Vec<Copy>> {
        self.links.share_with(from);
        let (changed, copies) = (self.store)
            .write(move |tx| Ok((copies::record(tx, edits)?, copies::list(tx)?)))
            .await?;
        if !changed.is_empty() {
            self.send_copies(daemon, from, changed).await?;
        }
        Ok(copies)
    }

    /// As the hub: send `changed` to every machine but `from`, its own
    /// included, without waiting for them.
    async fn send_copies(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        from: &MemberId,
        changed: Vec<Copy>,
    ) -> Result<()> {
        let own = self.own_member().await?;
        for (member, calls) in self.links.sharing() {
            if &member == from {
                continue;
            }
            let copies = changed.clone();
            tokio::spawn(async move {
                if let Err(error) = send(&calls, copies).await {
                    log!("hub: could not send a machine the shared config: {error:#}");
                }
            });
        }
        if from != &own {
            let (hub, daemon) = (self.clone(), daemon.clone());
            tokio::spawn(async move {
                let Some(_operation) = hub.gates.operation_unless_exiting().await else {
                    return;
                };
                if let Err(error) = hub.take_sent_copies(&*daemon, true, changed).await {
                    log!("hub: could not take the shared config here: {error:#}");
                }
            });
        }
        Ok(())
    }
}

/// Send the machine whose link `calls` serves the `copies` that changed.
async fn send(calls: &Calls, copies: Vec<Copy>) -> Result<()> {
    match calls.call(Call::SharedFiles { copies }).await? {
        Reply::Delivered => Ok(()),
        reply => Err(anyhow!("the machine answered with an unexpected {reply:?}")),
    }
}

#[cfg(test)]
mod tests;
