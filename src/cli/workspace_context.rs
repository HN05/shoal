//! Noninteractive workspace selection over a captured directory context.
use std::path::Path;

use crate::model::Workspace;

#[derive(Clone, Copy)]
pub enum ScopeOrder {
    BeforeDirectory,
    AfterDirectory,
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
    /// without fetching a list; completion requires a known name or ID here.
    pub fn resolve(
        &self,
        explicit: Option<&str>,
        scoped: bool,
        order: ScopeOrder,
    ) -> Option<&'a Workspace> {
        if let Some(selector) = explicit {
            return self
                .workspaces
                .iter()
                .find(|workspace| workspace.name == selector || workspace.id == selector);
        }
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
