//! The `root_dir` that holds repository directories: preparing it, which
//! keeps it out of everything else Shoal records, and reserving a
//! repository's directory in it.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use shoal_protocol::model::{Repository, Workspace};

use crate::{Repositories, spotlight};

/// The configured `root_dir` and how preparing it treats it.
pub struct ConfiguredRoot {
    pub path: PathBuf,
    pub layout: spotlight::Layout,
}

impl Repositories {
    /// The configured `root_dir`, taken as registration finds it.
    pub(crate) fn configured_root(&self) -> Result<ConfiguredRoot> {
        Ok(ConfiguredRoot {
            path: self.config().root_dir(&self.paths)?,
            layout: spotlight::Layout::AsFound,
        })
    }

    /// The configured `root_dir`, where a chat without a repository runs,
    /// prepared as registering a repository prepares it, so a root the
    /// chat creates first takes the layout registration would give it.
    pub async fn prepare_chat_root(&self) -> Result<PathBuf> {
        let _registry = self.gates.lock_registry().await;
        let repositories = self.list().await?;
        let workspaces = self.workspaces().await?;
        let recorded = Recorded {
            repositories: &repositories,
            workspaces: &workspaces,
        };
        let root = self.configured_root()?;
        let state = fs::canonicalize(&self.paths.state)?;
        prepare_root(&root, &state, &recorded, None).await?;
        Ok(root.path)
    }
}

/// What Shoal records, which a root must stay out of.
pub struct Recorded<'a> {
    pub repositories: &'a [Repository],
    pub workspaces: &'a [Workspace],
}

/// The canonical, existing directory that holds repository directories: the
/// `root_dir`, or where its layout keeps them. Worktrees must stay out of
/// Shoal's state, every checkout, including the one `checkout` registers,
/// every repository directory and every workspace, and the daemon must be
/// able to write there, so a root it cannot write or that is or resolves
/// inside one of them is refused before anything is created or linked.
pub async fn prepare_root(
    root: &ConfiguredRoot,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<PathBuf> {
    let plan = plan_root(root, state, recorded, checkout).await?;
    plan.apply()?;
    plan.destination()
}

/// What [`prepare_root`] would change, refused as it would refuse, without
/// changing anything.
pub(crate) async fn plan_root(
    root: &ConfiguredRoot,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<spotlight::Plan> {
    let plan = spotlight::plan(&root.path, root.layout)?;
    for location in plan.locations()? {
        ensure_outside(&location, state, recorded, checkout)?;
        shoal_daemon_base::writable::ensure_writable(&location).await?;
    }
    Ok(plan)
}

/// Refuse a root `location` inside Shoal's state, a checkout, a repository
/// directory or a workspace.
fn ensure_outside(
    location: &Path,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<()> {
    let repositories = recorded.repositories;
    ensure!(
        !location.starts_with(state),
        "root_dir {} is inside Shoal's state directory",
        location.display()
    );
    let checkouts = repositories.iter().map(|repo| repo.path.as_path());
    for checkout in checkouts.chain(checkout) {
        ensure!(
            !location.starts_with(checkout),
            "root_dir {} is inside the repository checkout {}",
            location.display(),
            checkout.display()
        );
    }
    for directory in repositories
        .iter()
        .map(|repo| repo.workspaces_dir.as_path())
    {
        ensure!(
            !location.starts_with(directory),
            "root_dir {} is inside the repository directory {}",
            location.display(),
            directory.display()
        );
    }
    if let Some(workspace) = recorded
        .workspaces
        .iter()
        .find(|workspace| location.starts_with(&workspace.path))
    {
        bail!(
            "root_dir {} is inside the workspace {}",
            location.display(),
            workspace.name
        );
    }
    Ok(())
}

/// Reserve `<root>/<name>` (suffixed `-2`, `-3`, ... when occupied or recorded)
/// by creating it, so concurrent daemons cannot share one directory. A
/// candidate that is, holds or lies in a recorded checkout, repository
/// directory or one of `workspaces` is taken, even when that is gone.
pub(crate) fn reserve_directory(
    root: &Path,
    name: &str,
    repositories: &[Repository],
    workspaces: &[PathBuf],
) -> Result<PathBuf> {
    let recorded = || {
        repositories
            .iter()
            .flat_map(|repo| [&repo.path, &repo.workspaces_dir])
            .chain(workspaces)
    };
    // Every candidate would lie in it, whatever the suffix.
    if let Some(owner) = recorded().find(|owned| root.starts_with(owned)) {
        bail!(
            "root_dir {} is inside {}, which Shoal already records",
            root.display(),
            owner.display()
        );
    }
    for suffix in 1_u64.. {
        let path = root.join(if suffix == 1 {
            name.to_owned()
        } else {
            format!("{name}-{suffix}")
        });
        // A missing directory still owns its recorded path.
        if recorded().any(|owned| owned.starts_with(&path) || path.starts_with(owned)) {
            continue;
        }
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reserve repository directory {}", path.display()));
            }
        }
    }
    bail!("repository directory suffixes exhausted")
}

#[cfg(test)]
pub(crate) mod tests;
