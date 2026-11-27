use shoal_core::config::Config;
use shoal_protocol::model::Repository;

use super::super::summaries::Origins;
use crate::stand_in::{self, Registered, StandIn};

/// The repository `id` cloned from `url`, with the local config `local`.
fn registered(id: &str, url: &str, local: Option<&str>) -> Registered {
    Registered {
        repository: Repository {
            id: id.into(),
            path: format!("/repositories/{id}/.checkout").into(),
            source: url.into(),
            last_used: 0,
            name: Some(id.into()),
            workspaces_dir: format!("/repositories/{id}").into(),
            contribution: false,
            shared_url: None,
        },
        local_config: local.map(Into::into),
    }
}

#[tokio::test]
async fn a_machine_launches_an_agent_its_repositorys_config_defines_only_there() {
    let root = tempfile::tempdir().unwrap();
    let hub = stand_in::hub(&root, Config::default()).await;
    let reviewer = "[agents.reviewer]\ncommand = ['sh']\n";
    let daemon = StandIn::registering(vec![
        registered("app", "https://example.com/owner/app.git", Some(reviewer)),
        registered("site", "https://example.com/owner/site.git", None),
    ]);
    let launchable = (hub.launchable(&daemon, &mut Origins::default()))
        .await
        .unwrap();
    let launches = |url: &str| {
        let origin = shoal_protocol::repository::url_key(url);
        let registered = launchable.repositories.iter().find(|r| r.origin == origin);
        registered.unwrap().agents.contains(&"reviewer".to_owned())
    };
    assert!(launches("https://example.com/owner/app.git"));
    assert!(!launches("https://example.com/owner/site.git"));
    assert!(!launchable.agents.contains(&"reviewer".to_owned()));
}
