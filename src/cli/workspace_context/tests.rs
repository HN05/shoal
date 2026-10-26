use super::*;
use crate::state::WorkspaceState;

fn workspace(name: &str, path: &Path) -> Workspace {
    Workspace {
        holds: Vec::new(),
        review: Vec::new(),
        agent_state: None,
        base_workspace: None,
        stacked_workspaces: Vec::new(),
        parent_workspace: None,
        child_workspaces: Vec::new(),
        chat: None,
        links: crate::model::WorkspaceLinks::default(),
        running: false,
        done: false,
        usage: None,
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
    for order in [ScopeOrder::BeforeDirectory, ScopeOrder::AfterDirectory] {
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
        context
            .resolve(None, true, ScopeOrder::BeforeDirectory)
            .unwrap()
            .name,
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
    for order in [ScopeOrder::BeforeDirectory, ScopeOrder::AfterDirectory] {
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
            .resolve(None, false, ScopeOrder::BeforeDirectory)
            .unwrap()
            .name,
        "inner"
    );
    // Adapters retain the choice of canonical versus raw cwd.
    let raw = alias.join("nested");
    let context = WorkspaceContext::from_directory(&workspaces, Some(&raw));
    assert!(
        context
            .resolve(None, false, ScopeOrder::BeforeDirectory)
            .is_none()
    );
    let sibling = root.path().join("outer-other");
    let context = WorkspaceContext::from_directory(&workspaces, Some(&sibling));
    assert!(
        context
            .resolve(None, false, ScopeOrder::BeforeDirectory)
            .is_none()
    );
}
