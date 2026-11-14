use tokio::net::UnixStream;

use super::super::super::answer;
use super::*;
use crate::client::space_names::SpaceSelector;
use crate::{daemon::workspace_files::Attacher, protocol::Response};

#[tokio::test]
async fn item_waits_close_unanswered_while_an_update_restarts_the_daemon() {
    use tokio::io::AsyncReadExt;
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "wait".into(), None, None, None)
        .await
        .unwrap();
    manager
        .send_space_message(
            &SpaceSelector::id(&workspace.id),
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
            let updates = pr_updates(
                manager.clone(),
                SpaceSelector::from(workspace.as_str()),
                Some(60),
            );
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

/// A Forgejo where PR 7 was opened by the review account's login and PR 8
/// by someone else, which accepts every review.
fn reviewed_forge() -> crate::forge::test_support::Server {
    crate::forge::test_support::Server::start(|request| {
        let pull = |number: u64, author: &str| {
            serde_json::json!({
                "number": number, "title": "Fix", "state": "open", "user": {"login": author},
                "head": {"ref": "topic", "sha": "b".repeat(40)}, "base": {"ref": "main"},
            })
        };
        let path = request.line.split_whitespace().nth(1).unwrap_or_default();
        let body = match path {
            "/api/v1/user" => serde_json::json!({"login": "reviewer"}),
            "/api/v1/repos/team/repo/pulls/7" => pull(7, "reviewer"),
            "/api/v1/repos/team/repo/pulls/8" => pull(8, "someone"),
            _ if request.line.starts_with("POST") => serde_json::json!({"id": 1}),
            _ => serde_json::json!([]),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

/// The `pr review --approve` of PR `number` from the chat `id`.
fn approval(id: &crate::client::space_ids::SpaceId, number: &str) -> ItemMethod {
    use crate::forge::review::submit::{NewReview, ReviewVerdict};
    ItemMethod::ItemAction {
        context: ItemContext::Chat(SpaceSelector::id(id)),
        kind: crate::forge::items::link::ItemKind::Pr,
        item: Some(number.parse().unwrap()),
        action: Action::Review(NewReview {
            verdict: ReviewVerdict::Approve,
            body: "Looks right".into(),
            comments: Vec::new(),
            commit: None,
        }),
        confirmed: None,
    }
}

#[tokio::test]
async fn agents_approve_prs_as_the_review_account_as_pr_review_allows() {
    use crate::daemon::{approvals::Undecided, test_support};

    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let server = reviewed_forge();
    let host = &server.host;
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    let origin = format!("http://{host}/team/repo");
    test_support::git(&path, &["remote", "add", "origin", &origin]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let forge =
        format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nagent_token = \"agent\"\n");
    let direct = "[modes.plan.permissions]\npr_review = \"direct\"\n";
    test_support::configure(&manager, &format!("{forge}{direct}")).await;
    let reviewing = test_support::chat(&manager, "reviewing", "plan", Some(&repo), None).await;
    let proposing = test_support::chat(&manager, "proposing", "chat", Some(&repo), None).await;
    let caller = |id: &crate::client::space_ids::SpaceId| Caller {
        space_id: id.clone(),
        execution: None,
        granted: None,
    };
    let approve = async |id, number| {
        let mut method = approval(id, number);
        method.scope(&manager, &caller(id)).await?;
        handle(&manager, method, Some(&caller(id))).await
    };
    let posted = || {
        (server.received().into_iter())
            .filter(|request| request.line.starts_with("POST"))
            .map(|request| request.header("Authorization").map(str::to_owned))
            .collect::<Vec<_>>()
    };

    // Without a review_token nothing is sent.
    let refused = format!("{:#}", approve(&reviewing.id, "8").await.err().unwrap());
    assert!(refused.contains("review_token"), "{refused}");
    assert!(server.received().is_empty());

    let review = "review_token = \"review\"\n";
    test_support::configure(&manager, &format!("{forge}{review}{direct}")).await;
    // The review account opened PR 7, so its approval is refused unsent.
    let refused = format!("{:#}", approve(&reviewing.id, "7").await.err().unwrap());
    assert!(refused.contains("review_token"), "{refused}");
    assert!(posted().is_empty());

    // Proposing asks approval before posting.
    let proposed = approve(&proposing.id, "8").await.err().unwrap();
    let Ok(Undecided(request)) = proposed.downcast::<Undecided>() else {
        panic!("expected an approval request");
    };
    let key = crate::config::permissions::PermissionKey::PrReview;
    assert_eq!(
        request.target,
        crate::daemon::approvals::Target::Action(key)
    );
    assert!(request.name.contains(&"b".repeat(40)), "{}", request.name);
    assert!(posted().is_empty());

    // Once approved, the same review is posted as the review account, as
    // a direct permission posts it at once.
    let decision =
        manager.decide_approval_as(None, request.number, true, None, Default::default(), None);
    decision.await.unwrap();
    approve(&proposing.id, "8").await.unwrap();
    approve(&reviewing.id, "8").await.unwrap();
    let review = Some("token review".to_owned());
    assert_eq!(posted(), [review.clone(), review]);
}
