use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use shoal_core::config::values::{Remote, Unanswered};

use super::*;
use crate::daemon::test_support::{global_config, manager, repository};

/// Another machine, which prints `value` and records what it was asked.
#[derive(Default)]
struct Desk {
    asked: Mutex<Vec<(String, String, Vec<String>)>>,
}

impl Remote for Desk {
    fn run<'a>(
        &'a self,
        machine: &'a str,
        setting: &'a str,
        command: &'a [String],
    ) -> BoxFuture<'a, Result<String, Unanswered>> {
        let asked = (machine.into(), setting.into(), command.to_vec());
        self.asked.lock().unwrap().push(asked);
        Box::pin(async { Ok("sk-desk".into()) })
    }
}

fn on_desk() -> Value {
    Value::Cmd {
        cmd: vec!["security".into()],
        on: Some("desk".into()),
    }
}

/// A daemon whose proxy `gateway` takes its key from `desk`, and a
/// workspace whose processes ask for it.
async fn daemon() -> (tempfile::TempDir, Arc<Manager>, Arc<Desk>, SpaceSelector) {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config(
        "[machine.proxies.gateway]\nupstream = 'http://127.0.0.1:1'\n\
         key = { cmd = ['security'], on = 'desk' }\n\
         [machine.proxies.local]\nupstream = 'http://127.0.0.1:1'\nkey = { cmd = ['security'] }\n",
    ));
    let desk = Arc::new(Desk::default());
    manager.config_state.values().connect(desk.clone());
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.repositories)
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    let worker = manager.named("worker").await;
    (temp, manager, desk, worker)
}

#[tokio::test]
async fn a_proxy_key_another_machine_prints_comes_through_the_daemon() {
    let (_temp, manager, desk, worker) = daemon().await;
    let key = ValueKey::ProxyKey { key: on_desk() };

    let value = manager.config_value(&worker, key, None).await.unwrap();

    assert_eq!(value.0, "sk-desk");
    let asked = desk.asked.lock().unwrap().clone();
    let expected = ("desk".into(), "machine.proxies.gateway.key".into());
    assert_eq!(asked, [(expected.0, expected.1, vec!["security".into()])]);
}

#[tokio::test]
async fn the_daemon_runs_only_a_command_its_config_gives() {
    let (_temp, manager, desk, worker) = daemon().await;
    for key in [
        Value::Cmd {
            cmd: vec!["rm".into(), "-rf".into(), "/".into()],
            on: Some("desk".into()),
        },
        Value::Cmd {
            cmd: vec!["security".into()],
            on: None,
        },
    ] {
        let key = ValueKey::ProxyKey { key };
        assert!(manager.config_value(&worker, key, None).await.is_err());
    }
    assert!(desk.asked.lock().unwrap().is_empty());
}
