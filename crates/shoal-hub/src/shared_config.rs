//! The shared config across a hub's machines: `shared.toml` and the mode
//! and rule files beside it, shared as [`SharedFiles`]. A machine sends the
//! hub its edits once the files are valid, and writes the copies the hub
//! sends; a copy that replaces an edit the hub never got leaves that edit
//! as a backup, with an alert.
use std::sync::Arc;

use futures_util::{FutureExt as _, future::BoxFuture};
use shoal_core::config::Shared;
use shoal_daemon_base::notifications::About;
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        shared_config::FileName,
        sharing::{Copies, Edits},
    },
};

use crate::{
    Hub,
    daemon::Daemon,
    log,
    sharing::{Agreed, Copy, Edit, Items, Kind, Sharing},
};

mod local;

/// The shared config's files, by name, with their text.
pub(crate) struct SharedFiles;

/// A machine's side of the shared files.
#[derive(Default)]
pub(super) struct Files {
    sharing: Sharing,
    /// The problem with the shared files the user last heard of, which
    /// keeps them from the hub.
    problem: std::sync::Mutex<Option<String>>,
}

impl Kind for SharedFiles {
    type Key = FileName;
    type Value = String;

    const NAME: &'static str = "file";

    fn sharing(hub: &Hub) -> &Sharing {
        &hub.shared_files.sharing
    }

    fn edits(edits: Vec<Edit<Self>>) -> Edits {
        Edits::Files(edits)
    }

    fn copies(copies: Vec<Copy<Self>>) -> Copies {
        Copies::Files(copies)
    }

    fn of(copies: Copies) -> Option<Vec<Copy<Self>>> {
        match copies {
            Copies::Files(copies) => Some(copies),
            Copies::Repositories(_) => None,
        }
    }

    fn items<'a>(
        hub: &'a Hub,
        _: &'a dyn Daemon,
        _: &'a Agreed<Self>,
    ) -> BoxFuture<'a, anyhow::Result<Items<Self>>> {
        let paths = hub.paths.clone();
        async move { tokio::task::spawn_blocking(move || Shared::files(&paths)).await? }.boxed()
    }

    fn sendable<'a>(
        hub: &'a Hub,
        daemon: &'a dyn Daemon,
        edits: Vec<Edit<Self>>,
    ) -> BoxFuture<'a, Vec<Edit<Self>>> {
        async move {
            match hub.shared_files_valid(daemon, !edits.is_empty()).await {
                true => edits,
                false => Vec::new(),
            }
        }
        .boxed()
    }

    fn take<'a>(
        hub: &'a Arc<Hub>,
        daemon: &'a Arc<dyn Daemon>,
        _: &'a shoal_protocol::hub::PublicKey,
        copies: Vec<Copy<Self>>,
        agreed: &'a Agreed<Self>,
        sent: &'a [Edit<Self>],
    ) -> BoxFuture<'a, anyhow::Result<Vec<Copy<Self>>>> {
        hub.take_file_copies(&**daemon, copies, agreed, sent)
            .boxed()
    }
}

impl Hub {
    /// Write the hub's `copies` into this machine's files, given the copies
    /// it `agreed` on with the hub and the edits it `sent` for them, then
    /// load the config they make and tell the user of each edit a copy
    /// replaced; the copies it agrees on now.
    async fn take_file_copies(
        &self,
        daemon: &dyn Daemon,
        copies: Vec<Copy<SharedFiles>>,
        agreed: &Agreed<SharedFiles>,
        sent: &[Edit<SharedFiles>],
    ) -> anyhow::Result<Vec<Copy<SharedFiles>>> {
        let paths = self.paths.clone();
        let (agreed, sent) = (agreed.clone(), sent.to_vec());
        let taken = move || local::take(&paths, copies, &agreed, &sent);
        let taken = tokio::task::spawn_blocking(taken).await??;
        if taken.wrote {
            self.reload_shared(daemon).await;
        }
        if !taken.kept.is_empty() {
            self.tell_kept(daemon, &taken.kept).await;
        }
        Ok(taken.agreed)
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

    /// Whether this machine's `edited` shared files may go to the hub: once
    /// they load, telling the user once of each new problem that keeps them
    /// from it. Without edits they hold the copies agreed on, which loaded,
    /// so the problem the user last heard of is gone.
    async fn shared_files_valid(&self, daemon: &dyn Daemon, edited: bool) -> bool {
        let problem_told = &self.shared_files.problem;
        if !edited {
            *problem_told.lock().unwrap() = None;
            return true;
        }
        let paths = self.paths.clone();
        let loaded = tokio::task::spawn_blocking(move || Shared::load(&paths)).await;
        let problem = match loaded {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => Some(format!("{error:#}")),
        };
        let told = std::mem::replace(&mut *problem_told.lock().unwrap(), problem.clone());
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
}

#[cfg(test)]
mod tests;
