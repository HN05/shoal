use std::sync::Arc;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::agents::GitCredential;

use crate::{
    callers::Caller, config::Config, config_state::ConfigState, forge_access::ForgeAccess,
};

#[tokio::test]
async fn only_scoped_callers_get_the_agent_token_of_a_host_with_one() {
    let temp = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(temp.path().join("state.db"))
        .await
        .unwrap();
    let config = Config::from_toml(
        "[machine.forges.\"git.example.com\"]\ntype = \"forgejo\"\nuser_token = \"user\"\nagent_token = \"agent\"\n\
         [machine.forges.\"code.example.com\"]\ntype = \"gitlab\"\nuser_token = \"user\"\n",
    )
    .unwrap();
    let access = ForgeAccess::new(store, Arc::default(), Arc::new(ConfigState::new(config)));
    let caller = Caller {
        space_id: SpaceId::generate(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };

    let scoped = access
        .agent_git_credential(Some(&caller), "git.example.com")
        .await
        .unwrap();
    let unscoped = access
        .agent_git_credential(None, "git.example.com")
        .await
        .unwrap();
    let user_token_only = access
        .agent_git_credential(Some(&caller), "code.example.com")
        .await
        .unwrap();

    assert_eq!(
        scoped,
        Some(GitCredential {
            username: "token".to_owned(),
            password: "agent".to_owned(),
        })
    );
    assert_eq!(unscoped, None);
    assert_eq!(user_token_only, None);
}
