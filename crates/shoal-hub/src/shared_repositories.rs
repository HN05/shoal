//! The repositories a hub's machines share, each by its origin. Git data
//! never goes through the hub: machines clone from the origin.
use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use futures_util::{FutureExt as _, future::BoxFuture};
use shoal_daemon_base::notifications::About;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        PublicKey,
        shared_repositories::{Origin, SharedRepository},
        sharing::{Copies, Edits},
    },
    model::Repository,
};

use crate::{
    Hub,
    daemon::Daemon,
    log,
    sharing::{
        Agreed, Copy, Edit, Items, Kind, Sharing,
        items::{Taking, taking},
    },
};

/// The shared repositories, by origin, as the machine that shared each
/// last has it.
pub(crate) struct SharedRepositories;

/// A machine's side of the shared repositories.
#[derive(Default)]
pub(super) struct Repositories {
    sharing: Sharing,
    cloning: Cloning,
}

impl Kind for SharedRepositories {
    type Key = Origin;
    type Value = SharedRepository;

    const NAME: &'static str = "repository";

    fn sharing(hub: &Hub) -> &Sharing {
        &hub.shared_repositories.sharing
    }

    fn edits(edits: Vec<Edit<Self>>) -> Edits {
        Edits::Repositories(edits)
    }

    fn copies(copies: Vec<Copy<Self>>) -> Copies {
        Copies::Repositories(copies)
    }

    fn of(copies: Copies) -> Option<Vec<Copy<Self>>> {
        match copies {
            Copies::Repositories(copies) => Some(copies),
            Copies::Files(_) | Copies::LocalConfigs(_) => None,
        }
    }

    fn items<'a>(
        _: &'a Hub,
        daemon: &'a dyn Daemon,
        agreed: &'a Agreed<Self>,
    ) -> BoxFuture<'a, Result<Items<Self>>> {
        async move { Ok(shared(&daemon.repositories().await?, agreed)) }.boxed()
    }

    fn sendable<'a>(
        _: &'a Hub,
        _: &'a dyn Daemon,
        edits: Vec<Edit<Self>>,
    ) -> BoxFuture<'a, Vec<Edit<Self>>> {
        std::future::ready(edits).boxed()
    }

    fn take<'a>(
        hub: &'a Arc<Hub>,
        daemon: &'a Arc<dyn Daemon>,
        key: &'a PublicKey,
        copies: Vec<Copy<Self>>,
        agreed: &'a Agreed<Self>,
        sent: &'a [Edit<Self>],
    ) -> BoxFuture<'a, Result<Vec<Copy<Self>>>> {
        hub.take_repository_copies(daemon, key, copies, agreed, sent)
            .boxed()
    }
}

/// The shared items `repositories`, the registrations here, hold: each
/// shared one by its origin, as the copy agreed on with the hub has it
/// where there is one, so a name or URL of its own here is no edit.
pub(crate) fn shared(
    repositories: &[Repository],
    agreed: &Agreed<SharedRepositories>,
) -> Items<SharedRepositories> {
    (repositories.iter())
        .filter_map(|repository| {
            let url = repository.shared_url.as_ref()?;
            let origin = Origin::of(url);
            let agreed = agreed.get(&origin).and_then(|copy| copy.value.clone());
            let value = agreed.unwrap_or_else(|| SharedRepository {
                url: url.clone(),
                name: repository.display_name().to_owned(),
            });
            Some((origin, value))
        })
        .collect()
}

/// What a copy that changes a machine's item does to its registrations.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applying {
    /// Share these registrations of its origin under its URL, or with
    /// none, stop sharing them.
    Share(Vec<RepositoryId>, Option<String>),
    /// Share this registration of its origin, which was not shared, under
    /// its URL.
    Join(RepositoryId, String),
    /// Register a clone of it, which no registration here has.
    Clone,
}

/// What the hub's `copy` does to `repositories`, the registrations here:
/// a shared copy shares those of its origin, a shared one first, and
/// clones it where there is none; one without a value unshares them.
pub(crate) async fn applying(
    copy: &Copy<SharedRepositories>,
    repositories: &[Repository],
) -> Applying {
    let of_origin = |repository: &&Repository| {
        (repository.shared_url.as_deref()).is_some_and(|url| Origin::of(url) == copy.key)
    };
    let shared: Vec<RepositoryId> = (repositories.iter())
        .filter(of_origin)
        .map(|repository| repository.id.clone())
        .collect();
    let Some(value) = &copy.value else {
        return Applying::Share(shared, None);
    };
    if !shared.is_empty() {
        return Applying::Share(shared, Some(value.url.clone()));
    }
    match shoal_core::repository::with_origin(repositories, copy.key.as_str()).await {
        Some(registered) => Applying::Join(registered.id.clone(), value.url.clone()),
        None => Applying::Clone,
    }
}

impl Hub {
    /// Look at the shared repositories at once, since they changed.
    pub fn shared_repositories_changed(&self) {
        self.shared_repositories.sharing.changed();
    }

    /// Take the hub's `copies` into this machine's registrations, given the
    /// copies it `agreed` on with the hub whose key is `key` and the edits
    /// it `sent` for them; the copies it agrees on now, without those it
    /// clones in the background, which it agrees on once cloned.
    async fn take_repository_copies(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        key: &PublicKey,
        copies: Vec<Copy<SharedRepositories>>,
        agreed: &Agreed<SharedRepositories>,
        sent: &[Edit<SharedRepositories>],
    ) -> Result<Vec<Copy<SharedRepositories>>> {
        let repositories = daemon.repositories().await?;
        let items = shared(&repositories, agreed);
        let mut taken = Vec::new();
        for copy in copies {
            let sent = sent.iter().find(|sent| sent.key == copy.key);
            let item = (items.get(&copy.key), agreed.get(&copy.key));
            match taking::<SharedRepositories>(&copy, item.0, item.1, sent) {
                Taking::Stale => continue,
                Taking::Agree => {}
                Taking::Write { .. } => {
                    let applied = applying(&copy, &repositories).await;
                    if !self.apply(daemon, key, applied, &copy).await? {
                        continue;
                    }
                }
            }
            taken.push(copy);
        }
        Ok(taken)
    }

    /// Change this machine's registrations as `applied` says, for the
    /// hub's `copy`, a registration that comes to be shared taking the
    /// hub's copy of its local config; whether the machine agrees on it
    /// now, or later, once the clone it starts is registered.
    async fn apply(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        key: &PublicKey,
        applied: Applying,
        copy: &Copy<SharedRepositories>,
    ) -> Result<bool> {
        match applied {
            Applying::Share(ids, url) => {
                for id in &ids {
                    daemon.share_repository_as(id, url.clone()).await?;
                }
                Ok(true)
            }
            Applying::Join(id, url) => {
                daemon.share_repository_as(&id, Some(url)).await?;
                (self.shared_registration_joined(&**daemon, &id, &copy.key)).await;
                Ok(true)
            }
            Applying::Clone => {
                self.clone_shared(daemon, key, copy.clone());
                Ok(false)
            }
        }
    }

    /// Register a clone of the shared repository `copy` names in the
    /// background, unless a clone of its origin is under way, which then
    /// takes this copy once it ends.
    fn clone_shared(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        key: &PublicKey,
        copy: Copy<SharedRepositories>,
    ) {
        let origin = copy.key.clone();
        if !self.shared_repositories.cloning.start(copy) {
            return;
        }
        let (hub, daemon, key) = (self.clone(), daemon.clone(), key.clone());
        tokio::spawn(async move {
            let Some(_operation) = hub.gates.operation_unless_exiting().await else {
                hub.shared_repositories.cloning.end(&origin);
                return;
            };
            hub.register_shared(&daemon, &key, origin).await;
        });
    }

    /// Register a clone of the shared repository of `origin`, as its
    /// newest copy names it, and share it as the copy agreed on with the
    /// hub whose key is `key`, telling the user when it cannot.
    async fn register_shared(&self, daemon: &Arc<dyn Daemon>, key: &PublicKey, origin: Origin) {
        let cloning = &self.shared_repositories.cloning;
        let Some(SharedRepository { url, name }) = cloning.newest(&origin) else {
            cloning.end(&origin);
            return;
        };
        let cloned = daemon.register_clone(url.clone(), name.clone()).await;
        let _round = self.shared_repositories.sharing.hold_rounds().await;
        let copy = cloning.end(&origin);
        let shared = match (cloned, copy) {
            (Ok(repository), Some(copy)) => self.share_clone(daemon, key, &repository, copy).await,
            (Ok(_), None) => Ok(()),
            (Err(error), _) => Err(error),
        };
        if let Err(error) = shared {
            self.tell_clone_failed(&**daemon, &name, &url, error).await;
        }
    }

    /// Share `repository`, a clone of the shared repository `copy` names,
    /// with the hub's copy of its local config, and agree on the copy with
    /// the hub whose key is `key`, unless the machine agreed on a later
    /// copy meanwhile, as one that unshares it.
    async fn share_clone(
        &self,
        daemon: &Arc<dyn Daemon>,
        key: &PublicKey,
        repository: &Repository,
        copy: Copy<SharedRepositories>,
    ) -> Result<()> {
        let agreed = self.agreed_copies::<SharedRepositories>(key).await?;
        if (agreed.get(&copy.key)).is_some_and(|agreed| agreed.revision >= copy.revision) {
            return Ok(());
        }
        let url = copy.value.as_ref().map(|value| value.url.clone());
        let joined = url.is_some();
        daemon.share_repository_as(&repository.id, url).await?;
        if joined {
            (self.shared_registration_joined(&**daemon, &repository.id, &copy.key)).await;
        }
        self.agree::<SharedRepositories>(key, vec![copy]).await
    }

    /// Tell the user that the shared repository `name` could not be
    /// registered as a clone of `url`.
    async fn tell_clone_failed(
        &self,
        daemon: &dyn Daemon,
        name: &str,
        url: &str,
        error: anyhow::Error,
    ) {
        let message = format!(
            "Could not register {name}, which another machine of the hub shares, as a clone of {url}: {error:#}; fix that, then run shoal repo add {url} --shared, or Shoal tries again when this machine next connects to the hub, which for the hub's own machine is when its daemon restarts"
        );
        log!("hub: {message}");
        (self.notify(
            daemon,
            About::named(None),
            AlertKind::SharedRepositoryFailed,
            message,
        ))
        .await;
    }
}

/// The newest copy of each origin whose clone is under way here.
#[derive(Default)]
struct Cloning(std::sync::Mutex<HashMap<Origin, Copy<SharedRepositories>>>);

impl Cloning {
    /// Take `copy` as its origin's newest; whether no clone of it was
    /// under way, which then starts.
    fn start(&self, copy: Copy<SharedRepositories>) -> bool {
        let mut cloning = self.0.lock().unwrap();
        cloning.insert(copy.key.clone(), copy).is_none()
    }

    /// What the newest copy of `origin` shares.
    fn newest(&self, origin: &Origin) -> Option<SharedRepository> {
        let cloning = self.0.lock().unwrap();
        cloning.get(origin).and_then(|copy| copy.value.clone())
    }

    /// End the clone of `origin`, returning its newest copy.
    fn end(&self, origin: &Origin) -> Option<Copy<SharedRepositories>> {
        self.0.lock().unwrap().remove(origin)
    }
}

#[cfg(test)]
mod tests;
