//! What a hub's machines share, item by item, of each [`Kind`]. Each
//! machine, the hub's own included, looks at its items every [`LOOK_EVERY`],
//! on each config reload and once told they changed, and sends the hub each item it edited since
//! it last agreed on that item with the hub, once they may go; on each link
//! it first reads the hub's copies. The hub keeps the last edit of each
//! item that reached it and sends it to every other machine, which takes
//! it.
use std::{
    collections::BTreeMap,
    convert::Infallible,
    fmt,
    str::FromStr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use futures_util::future::BoxFuture;
use serde::{Serialize, de::DeserializeOwned};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    PublicKey,
    link::{Call, Reply},
    sharing::{self, Copies, Edits},
};

use crate::{
    Hub, daemon::Daemon, log, membership, session::Calls, shared_config::SharedFiles,
    shared_repositories::SharedRepositories,
};

mod agreed;
pub(crate) mod copies;
pub(crate) mod items;
mod values;

/// How often a machine looks at its shared items for edits; a reload, or
/// word that they changed, looks at once.
const LOOK_EVERY: Duration = Duration::from_secs(10);

/// A kind of item a hub's machines share, under keys of its own.
pub(crate) trait Kind: Sized + Send + Sync + 'static {
    type Key: Clone + Ord + fmt::Debug + fmt::Display + FromStr<Err = anyhow::Error> + Send + Sync;
    type Value: Clone + PartialEq + fmt::Debug + Serialize + DeserializeOwned + Send + Sync;

    /// How the store and the links name the kind.
    const NAME: &'static str;

    /// This machine's side of the kind.
    fn sharing(hub: &Hub) -> &Sharing;

    /// `edits` as a link carries them.
    fn edits(edits: Vec<Edit<Self>>) -> Edits;

    /// `copies` as a link carries them.
    fn copies(copies: Vec<Copy<Self>>) -> Copies;

    /// The copies of this kind among `copies`, none when they are of
    /// another.
    fn of(copies: Copies) -> Option<Vec<Copy<Self>>>;

    /// This machine's items, given the copies it `agreed` on with the hub.
    fn items<'a>(
        hub: &'a Hub,
        daemon: &'a dyn Daemon,
        agreed: &'a Agreed<Self>,
    ) -> BoxFuture<'a, Result<Items<Self>>>;

    /// The `edits` of this machine's items that may go to the hub.
    fn sendable<'a>(
        hub: &'a Hub,
        daemon: &'a dyn Daemon,
        edits: Vec<Edit<Self>>,
    ) -> BoxFuture<'a, Vec<Edit<Self>>>;

    /// Take the copies of the hub whose key is `key` into this machine's
    /// items, given the copies it `agreed` on before and the edits it
    /// `sent` for them, returning the copies it agrees on now.
    fn take<'a>(
        hub: &'a Arc<Hub>,
        daemon: &'a Arc<dyn Daemon>,
        key: &'a PublicKey,
        copies: Vec<Copy<Self>>,
        agreed: &'a Agreed<Self>,
        sent: &'a [Edit<Self>],
    ) -> BoxFuture<'a, Result<Vec<Copy<Self>>>>;
}

/// A machine's edit of an item of `S`.
pub(crate) type Edit<S> = sharing::Edit<<S as Kind>::Key, <S as Kind>::Value>;

/// The hub's copy of an item of `S`.
pub(crate) type Copy<S> = sharing::Copy<<S as Kind>::Key, <S as Kind>::Value>;

/// A machine's items of `S`, by key.
pub(crate) type Items<S> = BTreeMap<<S as Kind>::Key, <S as Kind>::Value>;

/// The copies of `S` a machine agreed on with the hub, by key.
pub(crate) type Agreed<S> = BTreeMap<<S as Kind>::Key, Copy<S>>;

/// A machine's side of a kind of shared item.
#[derive(Default)]
pub(crate) struct Sharing {
    /// Held while the machine compares its items with the hub's copies and
    /// takes them, so a copy the hub sends waits for a round that sent an
    /// edit of its item.
    round: tokio::sync::Mutex<()>,
    /// Whether a copy the hub sent was not taken, so the next round reads
    /// the hub's copies again.
    missed: std::sync::atomic::AtomicBool,
    /// Wakes a round once the items change.
    changed: tokio::sync::Notify,
}

impl Sharing {
    /// Look at the items at once, since they changed.
    pub(crate) fn changed(&self) {
        self.changed.notify_one();
    }

    /// Hold off every round, which compares the items with the hub's
    /// copies and takes them, while the guard lives.
    pub(crate) async fn hold_rounds(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.round.lock().await
    }
}

/// Where a machine's edits go: over its link, or, on the hub's own
/// machine, to the hub as its own machine's.
pub(crate) enum To<'a> {
    Link(&'a Calls),
    Own(MemberId),
}

impl To<'_> {
    fn own(&self) -> bool {
        matches!(self, Self::Own(_))
    }
}

impl Hub {
    /// Share this machine's items over its link to the hub for as long as
    /// it stays open.
    pub(super) async fn keep_sharing(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        calls: &Calls,
    ) -> Infallible {
        self.keep_sharing_all(daemon, &To::Link(calls)).await
    }

    /// Share the hub's own machine's items while this daemon is the hub.
    pub(super) async fn keep_sharing_own(self: &Arc<Self>, daemon: &Arc<dyn Daemon>) {
        let mut reloaded = self.config.watch();
        loop {
            if self.is_hub(&**daemon) {
                match self.own_member().await {
                    Ok(own) => {
                        let own = To::Own(own);
                        tokio::select! {
                            never = self.keep_sharing_all(daemon, &own) => match never {},
                            () = self.hub_role_ended(&**daemon) => {}
                        }
                    }
                    Err(error) => {
                        log!("hub: could not share with the hub: {error:#}");
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

    /// Share this machine's items of every kind with `hub`.
    async fn keep_sharing_all(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
    ) -> Infallible {
        tokio::select! {
            never = self.keep_sharing_with::<SharedFiles>(daemon, hub) => never,
            never = self.keep_sharing_with::<SharedRepositories>(daemon, hub) => never,
        }
    }

    /// Share this machine's items of `S` with `hub`: read its copies first,
    /// then send each edit as it is seen.
    async fn keep_sharing_with<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
    ) -> Infallible {
        let mut reloaded = self.config.watch();
        let mut knows = false;
        loop {
            match self.share_round::<S>(daemon, hub, knows).await {
                // Items the hub has none of go at once.
                Ok(()) if !knows => {
                    knows = true;
                    continue;
                }
                Ok(()) => {}
                Err(error) => log!("hub: could not share the shared {}s: {error:#}", S::NAME),
            }
            tokio::select! {
                () = tokio::time::sleep(LOOK_EVERY) => {}
                () = S::sharing(self).changed.notified() => {}
                changed = reloaded.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }
        }
    }

    /// Send `hub` this machine's edits of `S`, once they may go, and take
    /// the copies it answers with; until the machine `knows` the hub's
    /// copies, or after it missed one, it reads them, sending only edits of
    /// items agreed on before until it knows them.
    pub(crate) async fn share_round<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
        knows: bool,
    ) -> Result<()> {
        let sharing = S::sharing(self);
        let _round = sharing.round.lock().await;
        let key = self.shared_hub_key(hub.own()).await?;
        let agreed = self.agreed_copies::<S>(&key).await?;
        let items = S::items(self, &**daemon, &agreed).await?;
        let edits = items::edits::<S>(&items, &agreed, knows);
        let edits = S::sendable(self, &**daemon, edits).await;
        let missed = sharing.missed.load(Ordering::SeqCst);
        if edits.is_empty() && knows && !missed {
            return Ok(());
        }
        let shared = self.exchange_copies::<S>(daemon, hub, &key, agreed, edits);
        let shared = shared.await;
        sharing.missed.store(shared.is_err(), Ordering::SeqCst);
        shared
    }

    /// Send `hub` `edits` and take the copies it answers with into this
    /// machine's items, which agreed on the copies `agreed` with the hub
    /// whose key is `key`.
    async fn exchange_copies<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
        key: &PublicKey,
        agreed: Agreed<S>,
        edits: Vec<Edit<S>>,
    ) -> Result<()> {
        let copies = self.share_with::<S>(daemon, hub, edits.clone()).await?;
        // Held so the daemon never exits between taking a copy and
        // recording that it agrees on it, as answering the hub's calls is.
        let Some(_operation) = self.gates.operation_unless_exiting().await else {
            bail!("the daemon is exiting");
        };
        self.take_copies::<S>(daemon, key, copies, agreed, edits)
            .await
    }

    /// Send `hub` `edits`, returning its copy of every item of `S`.
    async fn share_with<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        hub: &To<'_>,
        edits: Vec<Edit<S>>,
    ) -> Result<Vec<Copy<S>>> {
        match hub {
            To::Own(own) => self.take_shared_edits::<S>(daemon, own, edits).await,
            To::Link(calls) => {
                let edits = S::edits(edits);
                match calls.call(Call::Share { edits }).await? {
                    Reply::Shared(copies) => S::of(copies)
                        .ok_or_else(|| anyhow!("the hub answered with copies of another kind")),
                    reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
                }
            }
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
    /// when `own`; once that fails, the next round of their kind reads the
    /// hub's copies again.
    pub(super) async fn take_sent(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        own: bool,
        copies: Copies,
    ) -> Result<()> {
        match copies {
            Copies::Repositories(copies) => {
                (self.take_sent_copies::<SharedRepositories>(daemon, own, copies)).await
            }
            Copies::Files(copies) => {
                self.take_sent_copies::<SharedFiles>(daemon, own, copies)
                    .await
            }
        }
    }

    /// Take the `copies` of `S` the hub sends this machine, or its own
    /// machine when `own`.
    pub(crate) async fn take_sent_copies<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        own: bool,
        copies: Vec<Copy<S>>,
    ) -> Result<()> {
        let sharing = S::sharing(self);
        let _round = sharing.round.lock().await;
        let taken = async {
            let key = self.shared_hub_key(own).await?;
            let agreed = self.agreed_copies::<S>(&key).await?;
            self.take_copies::<S>(daemon, &key, copies, agreed, Vec::new())
                .await
        };
        let taken = taken.await;
        if taken.is_err() {
            sharing.missed.store(true, Ordering::SeqCst);
        }
        taken
    }

    /// Take the hub's `copies` into this machine's items, given the copies
    /// it `agreed` on with the hub whose key is `key` and the edits it
    /// `sent` for them, and record those it agrees on now.
    async fn take_copies<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        key: &PublicKey,
        copies: Vec<Copy<S>>,
        agreed: Agreed<S>,
        sent: Vec<Edit<S>>,
    ) -> Result<()> {
        let taken = S::take(self, daemon, key, copies, &agreed, &sent).await?;
        self.agree::<S>(key, taken).await
    }

    /// Record `copies` of `S` as agreed with the hub whose key is `key`.
    pub(crate) async fn agree<S: Kind>(&self, key: &PublicKey, copies: Vec<Copy<S>>) -> Result<()> {
        let key = key.clone();
        (self.store)
            .write(move |tx| agreed::record::<S>(tx, &key, &copies))
            .await
    }

    /// The copies of `S` this machine agreed on with the hub whose key is
    /// `key`.
    pub(crate) async fn agreed_copies<S: Kind>(&self, key: &PublicKey) -> Result<Agreed<S>> {
        let key = key.clone();
        self.store.read(move |tx| agreed::load::<S>(tx, &key)).await
    }

    /// As the hub: keep the `edits` of the machine `from`, of one kind,
    /// and answer with its copy of every item of that kind.
    pub(super) async fn take_edits(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        from: &MemberId,
        edits: Edits,
    ) -> Result<Copies> {
        Ok(match edits {
            Edits::Files(edits) => Copies::Files(
                self.take_shared_edits::<SharedFiles>(daemon, from, edits)
                    .await?,
            ),
            Edits::Repositories(edits) => Copies::Repositories(
                (self.take_shared_edits::<SharedRepositories>(daemon, from, edits)).await?,
            ),
        })
    }

    /// As the hub: keep the `edits` of `S` of the machine `from` that
    /// change its copies, send what they changed to every other machine,
    /// and return its copy of every item of `S`. From then on, `from`
    /// receives the copies of `S` others change.
    pub(crate) async fn take_shared_edits<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        from: &MemberId,
        edits: Vec<Edit<S>>,
    ) -> Result<Vec<Copy<S>>> {
        self.links.share_with(from, S::NAME);
        let (changed, copies) = (self.store)
            .write(move |tx| Ok((copies::record::<S>(tx, edits)?, copies::list::<S>(tx)?)))
            .await?;
        if !changed.is_empty() {
            self.send_copies::<S>(daemon, from, changed).await?;
        }
        Ok(copies)
    }

    /// As the hub: send `changed` to every machine but `from`, its own
    /// included, without waiting for them.
    async fn send_copies<S: Kind>(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        from: &MemberId,
        changed: Vec<Copy<S>>,
    ) -> Result<()> {
        let own = self.own_member().await?;
        for (member, calls) in self.links.sharing(S::NAME) {
            if &member == from {
                continue;
            }
            let copies = S::copies(changed.clone());
            tokio::spawn(async move {
                if let Err(error) = send(&calls, copies).await {
                    log!(
                        "hub: could not send a machine the shared {}s: {error:#}",
                        S::NAME
                    );
                }
            });
        }
        if from != &own {
            let (hub, daemon) = (self.clone(), daemon.clone());
            tokio::spawn(async move {
                let Some(_operation) = hub.gates.operation_unless_exiting().await else {
                    return;
                };
                if let Err(error) = hub.take_sent_copies::<S>(&daemon, true, changed).await {
                    log!(
                        "hub: could not take the shared {}s here: {error:#}",
                        S::NAME
                    );
                }
            });
        }
        Ok(())
    }
}

/// Send the machine whose link `calls` serves the `copies` that changed.
async fn send(calls: &Calls, copies: Copies) -> Result<()> {
    match calls.call(Call::Shared { copies }).await? {
        Reply::Delivered => Ok(()),
        reply => Err(anyhow!("the machine answered with an unexpected {reply:?}")),
    }
}
