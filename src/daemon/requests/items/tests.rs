use tokio::net::UnixStream;

use super::super::super::answer;
use super::*;
use crate::{daemon::workspace_files::Attacher, protocol::Response};

#[tokio::test]
async fn item_waits_close_unanswered_while_an_update_restarts_the_daemon() {
    use tokio::io::AsyncReadExt;
    let (root, manager) = crate::test_support::manager().await;
    let path = crate::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "wait".into(), None, None, None)
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let wait = |manager: Arc<Manager>| {
        let (client, stream) = UnixStream::pair().unwrap();
        let workspace = workspace.id.clone();
        let served = tokio::spawn(async move {
            let updates = pr_updates(manager.clone(), workspace, Some(60));
            answer::answer_wait(stream, 5, &manager, updates).await
        });
        (client, served)
    };
    let update = manager.gates.quiesce().await;
    let (mut client, served) = wait(manager.clone());
    served.await.unwrap().unwrap();
    let mut sent = Vec::new();
    client.read_to_end(&mut sent).await.unwrap();
    assert!(sent.is_empty(), "{}", String::from_utf8_lossy(&sent));
    drop(update);
    // The message stays pending for the wait on the replacement daemon.
    let (client, served) = wait(manager);
    served.await.unwrap().unwrap();
    let response: Response = protocol::read(&mut tokio::io::BufReader::new(client))
        .await
        .unwrap();
    let Body::PrUpdates(updates) = response.body else {
        panic!("expected item updates");
    };
    assert_eq!(updates.messages[0].message, "Rebase onto main");
}
