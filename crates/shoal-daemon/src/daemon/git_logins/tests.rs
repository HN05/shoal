use crate::{
    client::{agents::GitCredential, space_ids::SpaceId},
    daemon::{
        scope::Caller,
        test_support::{configure, manager},
    },
};

#[tokio::test]
async fn only_scoped_callers_get_the_agent_token_of_a_host_with_one() {
    let (_root, manager) = manager().await;
    configure(
        &manager,
        "[machine.forges.\"git.example.com\"]\ntype = \"forgejo\"\ntoken = \"user\"\nagent_token = \"agent\"\n\
         [machine.forges.\"code.example.com\"]\ntype = \"gitlab\"\ntoken = \"user\"\n",
    )
    .await;
    let caller = Caller {
        space_id: SpaceId::generate(),
        execution: None,
    };

    let scoped = manager
        .agent_git_credential(Some(&caller), "git.example.com")
        .await
        .unwrap();
    let unscoped = manager
        .agent_git_credential(None, "git.example.com")
        .await
        .unwrap();
    let user_token_only = manager
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
