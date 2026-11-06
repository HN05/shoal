use super::*;

/// A workspace on its own branch, whose agent the integrations' tests launch.
pub(super) const WORKSPACE: AgentWorkspace = AgentWorkspace {
    name: "fix-login",
    branch: Some("fix-login-redirect"),
};

#[test]
fn prompt_wakes_follow_how_each_integration_wakes() {
    assert_eq!(
        prompt_wake(IntegrationKind::Codex),
        Some(PromptWake::Always)
    );
    assert_eq!(
        prompt_wake(IntegrationKind::Claude),
        Some(PromptWake::AfterInterrupt)
    );
    assert_eq!(prompt_wake(IntegrationKind::None), None);
}
