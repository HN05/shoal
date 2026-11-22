//! The local configs of the repositories a hub's machines share, as
//! [`SharedLocalConfigs`], each by its repository's origin, under the
//! shared files' rules: a machine sends the hub each edit once the file
//! parses, and writes the copies the hub sends into its shared
//! registrations of that origin; a copy that replaces an edit the hub never
//! got leaves that edit as `config.toml.backup`, with an alert. A machine
//! agrees on the copies of origins it shares no registration of without
//! writing them, so that a registration that comes to be shared takes the
//! hub's copy, as a machine that joins the hub takes the shared files; one
//! whose origin it agreed on no copy of is no edit until the machine has
//! read the hub's copies, which may hold one on its way here. The local
//! config of a repository that is not shared, or no longer is, stays as it
//! is and is no edit.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::Result;
use futures_util::{FutureExt as _, future::BoxFuture};
use shoal_daemon_base::notifications::About;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        PublicKey,
        shared_repositories::Origin,
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
        items::{Taking, stale, taking},
    },
};

/// The local configs of the shared repositories, by origin, with their
/// text.
pub(crate) struct SharedLocalConfigs;

/// A machine's side of the shared local configs.
#[derive(Default)]
pub(super) struct LocalConfigs {
    sharing: Sharing,
    /// The problem with each shared local config the user last heard of,
    /// which keeps it from the hub.
    problems: std::sync::Mutex<BTreeMap<Origin, String>>,
    /// The origins of registrations that came to be shared while this
    /// machine agreed on no copy of their local config, which wait for it
    /// to read the hub's copies.
    joining: std::sync::Mutex<BTreeSet<Origin>>,
}

impl Kind for SharedLocalConfigs {
    type Key = Origin;
    type Value = String;

    const NAME: &'static str = "local config";

    fn sharing(hub: &Hub) -> &Sharing {
        &hub.shared_local_configs.sharing
    }

    fn edits(edits: Vec<Edit<Self>>) -> Edits {
        Edits::LocalConfigs(edits)
    }

    fn copies(copies: Vec<Copy<Self>>) -> Copies {
        Copies::LocalConfigs(copies)
    }

    fn of(copies: Copies) -> Option<Vec<Copy<Self>>> {
        match copies {
            Copies::LocalConfigs(copies) => Some(copies),
            Copies::Files(_) | Copies::Repositories(_) => None,
        }
    }

    fn items<'a>(
        hub: &'a Hub,
        daemon: &'a dyn Daemon,
        agreed: &'a Agreed<Self>,
    ) -> BoxFuture<'a, Result<Items<Self>>> {
        let joining = hub.shared_local_configs.joining.lock().unwrap().clone();
        async move { local_configs(daemon, agreed, &joining).await }.boxed()
    }

    fn sendable<'a>(
        hub: &'a Hub,
        daemon: &'a dyn Daemon,
        edits: Vec<Edit<Self>>,
    ) -> BoxFuture<'a, Vec<Edit<Self>>> {
        hub.valid_local_configs(daemon, edits).boxed()
    }

    fn take<'a>(
        hub: &'a Arc<Hub>,
        daemon: &'a Arc<dyn Daemon>,
        _: &'a PublicKey,
        copies: Vec<Copy<Self>>,
        agreed: &'a Agreed<Self>,
        sent: &'a [Edit<Self>],
    ) -> BoxFuture<'a, Result<Vec<Copy<Self>>>> {
        hub.take_local_config_copies(&**daemon, copies, agreed, sent)
            .boxed()
    }

    fn read_copies(hub: &Hub) {
        hub.shared_local_configs.copies_read();
    }
}

impl LocalConfigs {
    /// Have the file of `origin` wait for the hub's copies, which the next
    /// round reads at once.
    fn wait_for_copies(&self, origin: &Origin) {
        self.joining.lock().unwrap().insert(origin.clone());
        self.sharing.reread();
    }

    /// The machine read the hub's copies, so the files that waited for
    /// them go on the next round, which starts at once.
    fn copies_read(&self) {
        if !std::mem::take(&mut *self.joining.lock().unwrap()).is_empty() {
            self.sharing.changed();
        }
    }
}

/// This machine's shared registrations, by origin.
fn shared_registrations(repositories: Vec<Repository>) -> BTreeMap<Origin, Vec<Repository>> {
    let mut shared = BTreeMap::<Origin, Vec<Repository>>::new();
    for repository in repositories {
        if let Some(url) = &repository.shared_url {
            shared.entry(Origin::of(url)).or_default().push(repository);
        }
    }
    shared
}

/// This machine's shared local configs: the text of the local config of
/// each origin it shares, its first registration's where several share
/// one, and the copy it `agreed` on of each other origin, whose file is no
/// longer shared and so no edit. The origins `joining` wait for the hub's
/// copies, so their files are no edit yet either.
async fn local_configs(
    daemon: &dyn Daemon,
    agreed: &Agreed<SharedLocalConfigs>,
    joining: &BTreeSet<Origin>,
) -> Result<Items<SharedLocalConfigs>> {
    let mut shared = shared_registrations(daemon.repositories().await?);
    shared.retain(|origin, _| !joining.contains(origin));
    let agreed_value = |origin: &Origin| agreed.get(origin).and_then(|copy| copy.value.clone());
    let mut items: Items<SharedLocalConfigs> = (agreed.keys())
        .filter(|origin| !shared.contains_key(*origin))
        .filter_map(|origin| Some((origin.clone(), agreed_value(origin)?)))
        .collect();
    for (origin, registrations) in shared {
        let text = match daemon.local_config_text(&registrations[0].id).await {
            Ok(text) => text,
            Err(error) => {
                log!("hub: could not read a shared local config: {error:#}");
                agreed_value(&origin)
            }
        };
        if let Some(text) = text {
            items.insert(origin, text);
        }
    }
    Ok(items)
}

impl Hub {
    /// Look at the shared local configs at once, since one changed.
    pub fn shared_local_configs_changed(&self) {
        self.shared_local_configs.sharing.changed();
    }

    /// Have the shared registration `id` of `origin`, which has just come
    /// to be shared, take the hub's copy of its local config that this
    /// machine agreed on, as a machine that joins the hub takes the shared
    /// files: a file of its own that differs stays as its backup. Without
    /// one, its file waits for the hub's copies, which the next round reads
    /// at once.
    pub async fn shared_registration_joined(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
        origin: &Origin,
    ) {
        if let Err(error) = self.take_agreed_local_config(daemon, id, origin).await {
            log!("hub: could not give a shared repository the hub's local config: {error:#}");
        }
    }

    /// Write the copy of `origin`'s local config this machine agreed on
    /// with its hub into the registration `id`, telling the user of an edit
    /// it replaced.
    async fn take_agreed_local_config(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
        origin: &Origin,
    ) -> Result<()> {
        let Some(key) = self.sharing_hub_key(daemon).await? else {
            return Ok(());
        };
        let _round = self.shared_local_configs.sharing.hold_rounds().await;
        let agreed = self.agreed_copies::<SharedLocalConfigs>(&key).await?;
        let Some(copy) = agreed.get(origin) else {
            self.shared_local_configs.wait_for_copies(origin);
            return Ok(());
        };
        if let Some(kept) = take_local_config(daemon, id, copy, None, None).await? {
            self.tell_local_configs_kept(daemon, &[kept]).await;
        }
        Ok(())
    }

    /// Write the hub's `copies` into this machine's shared registrations of
    /// their origins, given the copies it `agreed` on with the hub and the
    /// edits it `sent` for them, telling the user of each edit a copy
    /// replaced; the copies it agrees on now, those of origins it shares
    /// none of included.
    async fn take_local_config_copies(
        &self,
        daemon: &dyn Daemon,
        copies: Vec<Copy<SharedLocalConfigs>>,
        agreed: &Agreed<SharedLocalConfigs>,
        sent: &[Edit<SharedLocalConfigs>],
    ) -> Result<Vec<Copy<SharedLocalConfigs>>> {
        let shared = shared_registrations(daemon.repositories().await?);
        let (mut taken, mut kept) = (Vec::new(), Vec::new());
        for copy in copies {
            let agreed = agreed.get(&copy.key);
            if stale::<SharedLocalConfigs>(&copy, agreed) {
                continue;
            }
            let sent = sent.iter().find(|sent| sent.key == copy.key);
            for repository in shared.get(&copy.key).into_iter().flatten() {
                let id = &repository.id;
                kept.extend(take_local_config(daemon, id, &copy, agreed, sent).await?);
            }
            taken.push(copy);
        }
        if !kept.is_empty() {
            self.tell_local_configs_kept(daemon, &kept).await;
        }
        Ok(taken)
    }

    /// Tell the user that the hub's copies replaced local config edits it
    /// never got, which stay at the backups `kept`.
    async fn tell_local_configs_kept(&self, daemon: &dyn Daemon, kept: &[PathBuf]) {
        let kept: Vec<String> = (kept.iter())
            .map(|path| path.display().to_string())
            .collect();
        let message = format!(
            "The hub's copy of a shared repository's local config replaced edits this machine had not sent it; they stay in {}",
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

    /// The `edits` of shared local configs that parse, telling the user
    /// once of each new problem that keeps one from the hub. A local config
    /// without an edit holds the copy agreed on, which parsed, so the
    /// problem the user last heard of with it is gone.
    async fn valid_local_configs(
        &self,
        daemon: &dyn Daemon,
        edits: Vec<Edit<SharedLocalConfigs>>,
    ) -> Vec<Edit<SharedLocalConfigs>> {
        let (mut valid, mut problems) = (Vec::new(), BTreeMap::new());
        for edit in edits {
            match edit.value.as_deref().and_then(problem) {
                Some(problem) => {
                    problems.insert(edit.key, problem);
                }
                None => valid.push(edit),
            }
        }
        let told = std::mem::replace(
            &mut *self.shared_local_configs.problems.lock().unwrap(),
            problems.clone(),
        );
        for (origin, problem) in problems {
            if told.get(&origin) != Some(&problem) {
                self.tell_invalid(daemon, &origin, &problem).await;
            }
        }
        valid
    }

    /// Tell the user that the local config of the repository of `origin`
    /// has `problem`, so this machine sends the hub none of its edits.
    async fn tell_invalid(&self, daemon: &dyn Daemon, origin: &Origin, problem: &str) {
        let message = format!(
            "The local config of the shared repository {origin} is invalid, so this machine sends the hub none of its edits: {problem}; fix it, and Shoal sends it"
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

/// Why the local config `text` does not parse, none when it does.
fn problem(text: &str) -> Option<String> {
    let parsed = shoal_core::config::repo::parse(text);
    parsed.err().map(|error| format!("{error:#}"))
}

/// Write the hub's `copy` into the local config of the registration `id`,
/// given the copy this machine `agreed` on before and the edit it `sent`
/// for this copy; the backup of the edit it replaced, if any.
async fn take_local_config(
    daemon: &dyn Daemon,
    id: &RepositoryId,
    copy: &Copy<SharedLocalConfigs>,
    agreed: Option<&Copy<SharedLocalConfigs>>,
    sent: Option<&Edit<SharedLocalConfigs>>,
) -> Result<Option<PathBuf>> {
    let file = daemon.local_config_text(id).await?;
    match taking::<SharedLocalConfigs>(copy, file.as_ref(), agreed, sent) {
        Taking::Stale | Taking::Agree => Ok(None),
        Taking::Write { keep } => {
            let text = copy.value.clone();
            daemon.take_local_config_copy(id, file, text, keep).await
        }
    }
}

#[cfg(test)]
mod tests;
