use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::test_support::{configure, git, manager, repository},
    forge::{
        items::{
            link::ItemKind,
            list::{Search, StateFilter},
        },
        test_support::Server,
    },
    protocol::ItemContext,
};

#[tokio::test]
async fn a_scoped_chat_lists_items_as_the_agent_account() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let server = Server::start(|_| (200, Vec::new(), r#"{"total_count":0,"items":[]}"#.into()));
    let host = &server.host;
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::GitHub);
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{host}/team/repo"),
        ],
    );
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{host}\"]\ntype = \"github\"\ntoken = \"user\"\nagent_token = \"agent\"\n"
        ),
    )
    .await;
    let context = ItemContext::Chat(SpaceSelector::id(&chat.id));
    let search = Search {
        state: StateFilter::Open,
        labels: Vec::new(),
        author: None,
        text: None,
        limit: 1,
    };
    for scoped in [true, false] {
        manager
            .list_items(&context, ItemKind::Issue, &search, None, scoped)
            .await
            .unwrap();
    }
    // The chat's agent reads as its account; the user's own command in the
    // chat with the user's login.
    let accounts: Vec<_> = (server.received().into_iter())
        .map(|request| request.header("Authorization").map(str::to_owned))
        .collect();
    assert_eq!(
        accounts,
        [
            Some("Bearer agent".to_owned()),
            Some("Bearer user".to_owned())
        ]
    );
}
