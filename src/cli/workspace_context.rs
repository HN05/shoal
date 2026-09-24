//! Noninteractive workspace selection over a captured directory context.
use std::path::Path;

use crate::model::Workspace;

#[derive(Clone, Copy)]
pub enum ScopeOrder {
    First,
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
            ScopeOrder::First if scoped => scope(),
            ScopeOrder::First => self.current,
            ScopeOrder::AfterDirectory => self.current.or_else(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorkspaceState;

    fn workspace(name: &str, path: &Path) -> Workspace {
        Workspace {
            id: format!("id-{name}"),
            repository_id: "repo".into(),
            name: name.into(),
            path: path.into(),
            branch: name.into(),
            state: WorkspaceState::Ready,
            error: None,
            base_commit: None,
            base_ref: None,
            git_dir: None,
            git_dir_id: None,
        }
    }

    #[test]
    fn explicit_names_and_ids_win_and_misses_do_not_fall_back() {
        let workspaces = [
            workspace("own", Path::new("/unused")),
            workspace("explicit", Path::new("/elsewhere")),
        ];
        let context = WorkspaceContext {
            workspaces: &workspaces,
            current: workspaces.first(),
        };
        for order in [ScopeOrder::First, ScopeOrder::AfterDirectory] {
            for scoped in [false, true] {
                for selector in ["explicit", "id-explicit"] {
                    assert_eq!(
                        context.resolve(Some(selector), scoped, order).unwrap().id,
                        "id-explicit"
                    );
                }
                assert!(context.resolve(Some("missing"), scoped, order).is_none());
            }
        }
    }

    #[test]
    fn scope_order_and_disabled_directory_inference_are_explicit() {
        let workspaces = [
            workspace("own", Path::new("/unused")),
            workspace("current", Path::new("/elsewhere")),
        ];
        // Deliberately unfiltered input makes the ordering observable. Production
        // lists are filtered by the daemon, not trusted as authorization here.
        let context = WorkspaceContext {
            workspaces: &workspaces,
            current: workspaces.get(1),
        };
        assert_eq!(
            context.resolve(None, true, ScopeOrder::First).unwrap().name,
            "own"
        );
        assert_eq!(
            context
                .resolve(None, true, ScopeOrder::AfterDirectory)
                .unwrap()
                .name,
            "current"
        );
        let outside = WorkspaceContext::from_directory(&workspaces, None);
        for order in [ScopeOrder::First, ScopeOrder::AfterDirectory] {
            assert_eq!(outside.resolve(None, true, order).unwrap().name, "own");
            assert!(outside.resolve(None, false, order).is_none());
            assert!(
                WorkspaceContext::from_directory(&[], None)
                    .resolve(None, true, order)
                    .is_none()
            );
        }
    }

    #[test]
    fn directory_capture_matches_nested_and_symlink_paths() {
        let root = tempfile::tempdir().unwrap();
        let outer = root.path().join("outer");
        let inner = outer.join("inner");
        let nested = inner.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&inner, &alias).unwrap();
        let workspaces = [workspace("outer", &outer), workspace("inner", &alias)];
        let cwd = std::fs::canonicalize(alias.join("nested")).unwrap();
        let context = WorkspaceContext::from_directory(&workspaces, Some(&cwd));
        assert_eq!(
            context
                .resolve(None, false, ScopeOrder::First)
                .unwrap()
                .name,
            "inner"
        );
        // Adapters retain the choice of canonical versus raw cwd.
        let raw = alias.join("nested");
        let context = WorkspaceContext::from_directory(&workspaces, Some(&raw));
        assert!(context.resolve(None, false, ScopeOrder::First).is_none());
        let sibling = root.path().join("outer-other");
        let context = WorkspaceContext::from_directory(&workspaces, Some(&sibling));
        assert!(context.resolve(None, false, ScopeOrder::First).is_none());
    }
}
