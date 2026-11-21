//! A machine's shared files against the copies it agreed on with the hub:
//! which of them hold edits to send, and what each copy the hub sends does
//! to them.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Result;
use shoal_core::{config::Shared, config_file::shared_files};
use shoal_protocol::{
    hub::shared_config::{Copy, Edit, FileName},
    paths::Paths,
};

/// The edits `on_disk`, this machine's shared files, hold against the
/// copies it `agreed` on with the hub: each file that differs from its
/// agreed copy, and, once it `knows` the hub's copies, each file without
/// one, which the hub has none of.
pub(super) fn edits(
    on_disk: &BTreeMap<FileName, String>,
    agreed: &BTreeMap<FileName, Copy>,
    knows: bool,
) -> Vec<Edit> {
    let files: BTreeSet<&FileName> = on_disk.keys().chain(agreed.keys()).collect();
    (files.into_iter())
        .filter_map(|file| {
            let text = on_disk.get(file);
            let edited = match agreed.get(file) {
                Some(agreed) => agreed.text.as_ref() != text,
                None => knows && text.is_some(),
            };
            edited.then(|| Edit {
                file: file.clone(),
                text: text.cloned(),
            })
        })
        .collect()
}

/// What a copy from the hub does to a machine's file.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Taking {
    /// Nothing: the machine agreed on this copy or a later one.
    Stale,
    /// The machine agrees on it, which its file already reads, or which
    /// is the edit it sent.
    Agree,
    /// It replaces the file, which stays as a backup when `keep`, since
    /// it holds an edit the hub never got.
    Write { keep: bool },
}

/// What the hub's `copy` does to the file that reads `on_disk`, given the
/// copy of it the machine `agreed` on before and the edit of it the
/// machine `sent` for this copy, if any.
pub(super) fn taking(
    copy: &Copy,
    on_disk: Option<&String>,
    agreed: Option<&Copy>,
    sent: Option<&Edit>,
) -> Taking {
    if agreed.is_some_and(|agreed| agreed.revision >= copy.revision) {
        return Taking::Stale;
    }
    // A file edited again since it was sent keeps that edit for the next
    // round to send.
    if on_disk == copy.text.as_ref() || sent.is_some_and(|sent| sent.text == copy.text) {
        return Taking::Agree;
    }
    let edited = match agreed {
        Some(agreed) => agreed.text.as_ref() != on_disk,
        None => on_disk.is_some(),
    };
    Taking::Write {
        keep: edited && on_disk.is_some(),
    }
}

/// What taking the hub's copies came to.
#[derive(Debug, Default)]
pub(super) struct Taken {
    /// The copies the machine now agrees on.
    pub agreed: Vec<Copy>,
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
    copies: Vec<Copy>,
    agreed: &BTreeMap<FileName, Copy>,
    sent: &[Edit],
) -> Result<Taken> {
    let held = shared_files::hold(paths)?;
    let on_disk = Shared::files(paths)?;
    let mut taken = Taken::default();
    for copy in copies {
        let sent = sent.iter().find(|sent| sent.file == copy.file);
        let file = (on_disk.get(&copy.file), agreed.get(&copy.file));
        match taking(&copy, file.0, file.1, sent) {
            Taking::Stale => continue,
            Taking::Agree => {}
            Taking::Write { keep } => {
                let text = copy.text.as_deref();
                taken
                    .kept
                    .extend(held.write(paths, &copy.file, text, keep)?);
                taken.wrote = true;
            }
        }
        taken.agreed.push(copy);
    }
    Ok(taken)
}

#[cfg(test)]
mod tests;
