use super::handle;
use crate::{
    daemon::{
        scope::Caller,
        test_support::{chat, manager, repository},
    },
    protocol::HubMethod,
};

fn caller(space: &shoal_ids::space_ids::SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

#[tokio::test]
async fn a_space_updates_the_hub_as_its_daemon_permission_allows() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let config = "[modes.plan.permissions]\ndaemon = 'direct'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let direct = chat(&manager, "direct", "plan", Some(&repo), None).await;
    let denied = chat(&manager, "denied", "worker", Some(&repo), None).await;

    let update = |space| {
        let manager = manager.clone();
        async move {
            let caller = caller(space);
            match handle(&manager, HubMethod::UpdateHub, Some(&caller)).await {
                Ok(_) => String::new(),
                Err(error) => format!("{error:#}"),
            }
        }
    };
    let refused = update(&denied.id).await;
    assert!(refused.contains("\"deny\""), "{refused}");
    // Past its permission, it finds this machine joined no hub.
    let allowed = update(&direct.id).await;
    assert!(allowed.contains("joined none"), "{allowed}");
}
