//! Writing the hub's copies of the shared files into a machine's own.
use std::path::PathBuf;

use anyhow::Result;
use shoal_core::{config::Shared, config_file::shared_files};
use shoal_protocol::paths::Paths;

use super::SharedFiles;
use crate::sharing::{
    Agreed, Copy, Edit,
    items::{Taking, taking},
};

/// What taking the hub's copies came to.
#[derive(Debug, Default)]
pub(super) struct Taken {
    /// The copies the machine now agrees on.
    pub agreed: Vec<Copy<SharedFiles>>,
    /// The backups of the files that held edits the hub never got.
    pub kept: Vec<PathBuf>,
    /// Whether a file changed.
    pub wrote: bool,
}

/// Take the hub's `copies` into this machine's shared files, given the
/// copies it `agreed` on before and the edits it `sent` for them, under
/// the shared config's edit lock.
pub(super) fn take(
    paths: &Paths,
    copies: Vec<Copy<SharedFiles>>,
    agreed: &Agreed<SharedFiles>,
    sent: &[Edit<SharedFiles>],
) -> Result<Taken> {
    let held = shared_files::hold(paths)?;
    let on_disk = Shared::files(paths)?;
    let mut taken = Taken::default();
    for copy in copies {
        let sent = sent.iter().find(|sent| sent.key == copy.key);
        let file = (on_disk.get(&copy.key), agreed.get(&copy.key));
        match taking::<SharedFiles>(&copy, file.0, file.1, sent) {
            Taking::Stale => continue,
            Taking::Agree => {}
            Taking::Write { keep } => {
                let text = copy.value.as_deref();
                taken.kept.extend(held.write(paths, &copy.key, text, keep)?);
                taken.wrote = true;
            }
        }
        taken.agreed.push(copy);
    }
    Ok(taken)
}
