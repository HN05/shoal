use std::sync::Arc;

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    chat_marks::{ChatMark, ChatMarked},
    space_names::SpaceSelector,
    threads::{Thread, ThreadKind, ThreadNumber},
};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope::{Caller, ScopedExecution},
        test_support::{caller_in, manager, repository, track_agent},
    },
    model::Workspace,
    protocol::TurnEnd,
};

/// A workspace `parent` whose agent runs as `agent`.
struct Tree {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    parent: Workspace,
    agent: ExecutionId,
}

async fn tree() -> Tree {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let agent = track_agent(&manager, &parent).await;
    Tree {
        _root: root,
        manager,
        parent,
        agent,
    }
}

impl Tree {
    /// The parent's agent, as the process it runs as.
    fn agent(&self) -> Caller {
        Caller {
            execution: Some(ScopedExecution {
                id: self.agent.clone(),
                kind: ExecutionKind::Command,
                agent_execution: None,
            }),
            ..caller_in(&SpaceSelector::id(&self.parent.id))
        }
    }

    /// End the parent's agent's turn with `message` as its final message.
    async fn end_turn(&self, message: &str) {
        let ending = TurnEnd {
            final_message: Some(message.into()),
            ..TurnEnd::default()
        };
        let selector = SpaceSelector::id(&self.parent.id);
        (self
            .manager
            .end_agent_turn(&selector, Some(self.agent.clone()), None, ending))
        .await
        .unwrap();
    }

    /// A thread the user opens with the parent.
    async fn thread(&self, name: &str) -> Thread {
        let parent = SpaceSelector::id(&self.parent.id);
        (self.manager.open_thread(
            None,
            name.into(),
            ThreadKind::Question,
            Some(&parent),
            vec![],
        ))
        .await
        .unwrap()
    }

    /// Mark the parent's agent's turn as posting in `thread`.
    async fn mark_post(&self, thread: &Thread) -> ChatMarked {
        let (agent, selector) = (self.agent(), SpaceSelector::id(&self.parent.id));
        let number = ThreadNumber::Full(thread.number);
        (self.manager.mark_chat_post(Some(&agent), &selector, number))
            .await
            .unwrap()
    }

    /// The texts of the posts `thread` has from the chat.
    async fn chat_posts(&self, thread: &Thread) -> Vec<String> {
        let number = ThreadNumber::Full(thread.number);
        let thread = self.manager.thread(None, number).await.unwrap();
        (thread.posts.into_iter())
            .filter(|post| post.in_chat)
            .map(|post| post.post.text)
            .collect()
    }
}

#[tokio::test]
async fn a_second_mark_in_a_turn_replaces_the_first_and_says_so() {
    let tree = tree().await;
    let (database, cache) = (tree.thread("Database?").await, tree.thread("Cache?").await);
    let first = tree.mark_post(&database).await;
    assert_eq!(first.replaced, None);
    let second = tree.mark_post(&cache).await;
    assert_eq!(
        (second.mark, second.replaced),
        (
            ChatMark::Post {
                thread: cache.number
            },
            Some(ChatMark::Post {
                thread: database.number
            })
        )
    );
    tree.end_turn("Redis").await;
    assert!(tree.chat_posts(&database).await.is_empty());
    assert_eq!(tree.chat_posts(&cache).await, ["Redis"]);
}
