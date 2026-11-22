//! Noninteractive workspace selection over a captured directory context.
use std::path::Path;

use crate::model::Workspace;
use anyhow::{Result, ensure};

use crate::{fsutil::current_dir::current_directory, scoping::Scoping};
use shoal_protocol::space_names::SpaceSelector;

#[derive(Clone, Copy)]
pub enum ScopeOrder {
    BeforeDirectory,
    AfterDirectory,
}

impl ScopeOrder {
    /// Whether selection with this order consults the current directory: a
    /// scoped caller that puts its scope first never does, so it needs no
    /// readable working directory.
    pub fn reads_directory(self, scoping: Scoping) -> bool {
        !(scoping == Scoping::Scoped && matches!(self, Self::BeforeDirectory))
    }
}

/// No workspace is named or current, where the caller takes none from a
/// picker.
#[derive(Debug)]
pub struct NoCurrentWorkspace;

impl std::fmt::Display for NoCurrentWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no current workspace; pass an explicit workspace")
    }
}

impl std::error::Error for NoCurrentWorkspace {}

/// The workspace this process acts in among `workspaces`, those it sees,
/// its own first when it is scoped, as `order` puts its scope and its
/// current directory, which it reads only where `order` needs it. A scoped
/// process whose workspace is not listed is refused; `None` means an
/// unscoped one runs in no workspace.
pub fn current(workspaces: &[Workspace], order: ScopeOrder) -> Result<Option<&Workspace>> {
    let scoping = Scoping::of_workspace();
    let cwd = match order.reads_directory(scoping) {
        true => current_directory()?,
        false => None,
    };
    let context = WorkspaceContext::from_directory(workspaces, cwd.as_deref());
    if let Some(workspace) = context.resolve(None, scoping, order) {
        return Ok(Some(workspace));
    }
    ensure!(
        scoping == Scoping::Unscoped,
        "scoped workspace is unavailable"
    );
    Ok(None)
}

pub struct WorkspaceContext<'a> {
    workspaces: &'a [Workspace],
    current: Option<&'a Workspace>,
}

impl<'a> WorkspaceContext<'a> {
    /// Capture filesystem matching separately from the pure selection policy.
    /// The adapter supplies cwd in its chosen form, or None to skip inference.
    pub fn from_directory(workspaces: &'a [Workspace], cwd: Option<&Path>) -> Self {
        Self {
            workspaces,
            current: cwd.and_then(|cwd| Workspace::innermost(workspaces, cwd)),
        }
    }

    /// Lists must already be daemon-filtered for scope. This selects context;
    /// authorization remains with the daemon. Explicit misses never infer a target.
    /// Execution adapters may forward explicit selectors directly to the daemon
    /// without fetching a list; completion requires a known number or ID here.
    pub fn resolve(
        &self,
        explicit: Option<&SpaceSelector>,
        scoping: Scoping,
        order: ScopeOrder,
    ) -> Option<&'a Workspace> {
        if let Some(selector) = explicit {
            return (self.workspaces.iter())
                .find(|workspace| selector.selects(&workspace.id, workspace.number));
        }
        let scoped = scoping == Scoping::Scoped;
        let scope = || scoped.then(|| self.workspaces.first()).flatten();
        match order {
            ScopeOrder::BeforeDirectory if scoped => scope(),
            ScopeOrder::BeforeDirectory => self.current,
            ScopeOrder::AfterDirectory => self.current.or_else(scope),
        }
    }
}

#[cfg(test)]
mod tests;
