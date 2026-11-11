use super::*;

#[test]
fn register_entry_accepts_only_registration() {
    for action in [RepoMenuAction::AddWorkspace, RepoMenuAction::Register] {
        assert_eq!(
            resolve_action(action, REGISTER_ENTRY).unwrap(),
            RepoMenuAction::Register
        );
    }
    for action in [
        RepoMenuAction::Rename,
        RepoMenuAction::Config,
        RepoMenuAction::Setup,
        RepoMenuAction::Delete,
    ] {
        assert!(resolve_action(action, REGISTER_ENTRY).is_err());
        assert_eq!(resolve_action(action, "repo-id").unwrap(), action);
    }
}
