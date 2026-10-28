//! Keeping `root_dir` out of Spotlight. Below a volume root Spotlight ignores
//! `.metadata_never_index` and skips only directories named `*.noindex` or
//! starting with `.`, so the root's contents live in `<parent>/.<name>.noindex`
//! and the configured root is a symlink to it, keeping its path the same on
//! every system.
use super::paths::canonical_with_missing_tail;
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

/// How preparing `root_dir` treats it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// An existing root as it is, a missing one in the default layout.
    AsFound,
    /// The configured directory itself.
    Regular,
    /// The excluded directory, which the configured root becomes a link to
    /// once it is empty.
    Excluded,
}

impl Layout {
    /// What a new root gets: excluded on macOS, where development builds
    /// leave the user's root to the installed Shoal.
    pub(crate) fn default_layout() -> Self {
        if cfg!(target_os = "macos") && crate::build::changes_user_files() {
            Self::Excluded
        } else {
            Self::Regular
        }
    }
}

/// What preparing a root changes, worked out before anything changes so that
/// every location can be checked first.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    /// Holds repository directories.
    directory: PathBuf,
    /// The root that becomes a link to `directory` once it is empty.
    link: Option<PathBuf>,
}

impl Plan {
    fn at(directory: PathBuf) -> Self {
        Self {
            directory,
            link: None,
        }
    }

    /// The canonical directory that holds repository directories once the
    /// plan is applied.
    pub(super) fn destination(&self) -> Result<PathBuf> {
        canonical_with_missing_tail(&self.directory)
    }

    /// Every location the plan creates or changes, as named and as resolved.
    pub(super) fn locations(&self) -> Result<Vec<PathBuf>> {
        let mut locations = Vec::new();
        for location in std::iter::once(&self.directory).chain(&self.link) {
            locations.push(canonical_with_missing_tail(location)?);
            locations.push(location.clone());
        }
        Ok(locations)
    }

    /// Make the changes, linking the root only once it is empty.
    pub(super) fn apply(&self) -> Result<()> {
        fs::create_dir_all(&self.directory).with_context(|| {
            format!(
                "create repository root directory {}",
                self.directory.display()
            )
        })?;
        if let Some(root) = &self.link {
            link_root(root, &self.directory)?;
        }
        Ok(())
    }
}

/// Plan the absolute root `root` in `layout`.
pub(super) fn plan(root: &Path, layout: Layout) -> Result<Plan> {
    let root = &with_canonical_parent(root)?;
    let resolved = canonical_with_missing_tail(root)?;
    let layout = match layout {
        Layout::AsFound if fs::symlink_metadata(root).is_ok() => return Ok(Plan::at(resolved)),
        Layout::AsFound => Layout::default_layout(),
        layout => layout,
    };
    Ok(match layout {
        Layout::Excluded if !skips(&resolved) => Plan {
            directory: excluded_directory(&resolved)?,
            link: Some(resolved),
        },
        _ => Plan::at(resolved),
    })
}

/// `root` with its parent resolved, so that a link at `root` stays one.
fn with_canonical_parent(root: &Path) -> Result<PathBuf> {
    match (root.parent(), root.file_name()) {
        (Some(parent), Some(name)) => Ok(canonical_with_missing_tail(parent)?.join(name)),
        _ => canonical_with_missing_tail(root),
    }
}

/// Whether Spotlight skips `path` because of its name or an ancestor's.
fn skips(path: &Path) -> bool {
    path.iter().any(|name| {
        let name = name.to_string_lossy();
        name.starts_with('.') || name.ends_with(".noindex")
    })
}

/// `<parent>/.<name>.noindex`, which holds `root`'s contents.
fn excluded_directory(root: &Path) -> Result<PathBuf> {
    let name = root.file_name().context("root_dir has no name")?;
    let mut hidden = OsString::from(".");
    hidden.push(name);
    hidden.push(".noindex");
    Ok(root.with_file_name(hidden))
}

/// Replace a missing or empty `root` with a symlink to its sibling
/// `directory`, keeping one that holds files.
fn link_root(root: &Path, directory: &Path) -> Result<()> {
    if entries(root)?.iter().all(|path| is_finder_metadata(path)) {
        let _ = fs::remove_file(root.join(".DS_Store"));
        // A file added meanwhile keeps the directory for a later call.
        let _ = fs::remove_dir(root);
    }
    let target = directory.file_name().context("root_dir has no name")?;
    match std::os::unix::fs::symlink(target, root) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error)
            .with_context(|| format!("link {} to {}", root.display(), directory.display())),
        _ => Ok(()),
    }
}

/// The paths in the directory `root`, none when it is missing.
fn entries(root: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(root) {
        Ok(entries) => entries
            .map(|entry| Ok(entry?.path()))
            .collect::<io::Result<_>>()
            .with_context(|| format!("read {}", root.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", root.display())),
    }
}

fn is_finder_metadata(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == ".DS_Store")
}

#[cfg(test)]
mod tests;
