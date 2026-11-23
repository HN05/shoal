use crate::daemon::scope::Access;
use crate::daemon::scope::Caller;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn an_agent_takes_a_space_over_only_as_its_admin_permission_allows() {
    use crate::config::permissions::PermissionKey;
    use crate::daemon::{
        approvals::{Target, Undecided},
        test_support::{chat, manager},
    };

    let caller = |space: &SpaceId| Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let (_temp, manager) = manager().await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let worker = chat(&manager, "worker", "chat", None, Some(&master.id)).await;
    let elsewhere = SpaceSelector::id(&SpaceId::from("elsewhere".to_owned()));
    // The user and a master session take it over at once; a chat below
    // asks its coordinator.
    (manager
        .spaces
        .ensure_takeover_allowed(&*manager, None, &elsewhere)
        .await)
        .unwrap();
    (manager
        .spaces
        .ensure_takeover_allowed(&*manager, Some(&caller(&master.id)), &elsewhere))
    .await
    .unwrap();
    let worker = caller(&worker.id);
    let asked = manager
        .spaces
        .ensure_takeover_allowed(&*manager, Some(&worker), &elsewhere);
    let Undecided(request) = asked.await.unwrap_err().downcast::<Undecided>().unwrap();
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
}
