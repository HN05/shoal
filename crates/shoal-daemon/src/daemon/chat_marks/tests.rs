use std::sync::Arc;

use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::{Alert, AlertKind},
    chat_marks::{ChatMark, ChatMarked, MarkedRecipients},
    files::NewFile,
    protocol::AlertTarget,
    space_names::SpaceSelector,
    threads::{Thread, ThreadKind, ThreadNumber},
};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope::{Caller, ScopedExecution},
        test_support::{
            away_on_offline_machine, caller_in, chat, held_calls, manager, repository, track_agent,
        },
    },
    model::Repository,
    protocol::TurnEnd,
};

/// An agent Shoal runs in a space, as the execution it runs as.
struct Agent {
    space: SpaceId,
    execution: ExecutionId,
}

impl Agent {
    /// The agent, as the process it runs as.
    fn caller(&self) -> Caller {
        Caller {
            execution: Some(ScopedExecution {
                id: self.execution.clone(),
                kind: ExecutionKind::Command,
                agent_execution: None,
            }),
            ..caller_in(&SpaceSelector::id(&self.space))
        }
    }

    /// End the agent's turn with `message` as its final message.
    async fn end_turn(&self, manager: &Manager, message: &str) {
        let ending = TurnEnd {
            final_message: Some(message.into()),
            ..TurnEnd::default()
        };
        let selector = SpaceSelector::id(&self.space);
        (manager.agents.end_agent_turn(
            manager,
            &selector,
            Some(self.execution.clone()),
            None,
            ending,
        ))
        .await
        .unwrap();
    }

    /// Mark the agent's turn as sending its final message to `to` with
    /// `attachments`.
    async fn mark_alert(
        &self,
        manager: &Manager,
        to: AlertTarget,
        attachments: Vec<NewFile>,
    ) -> ChatMarked {
        let caller = self.caller();
        (manager.mark_chat_alert(Some(&caller), to, attachments))
            .await
            .unwrap()
    }
}

/// A workspace `parent` whose agent runs, and a workspace below it whose
/// agent runs too.
struct Tree {
    root: tempfile::TempDir,
    manager: Arc<Manager>,
    repo: Repository,
    parent: Agent,
    child: Agent,
}

async fn tree() -> Tree {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let (parent, child) = (
        Agent {
            execution: track_agent(&manager, &parent).await,
            space: parent.id,
        },
        Agent {
            execution: track_agent(&manager, &child).await,
            space: child.id,
        },
    );
    Tree {
        root,
        manager,
        repo,
        parent,
        child,
    }
}

impl Tree {
    /// The agent of a chat below the parent.
    async fn chat_below(&self) -> Agent {
        let parent = Some(&self.parent.space);
        let chat = chat(&self.manager, "helper", "chat", Some(&self.repo), parent).await;
        let executions = self.manager.workspace_executions(&chat.id).await.unwrap();
        let execution = (executions.into_iter())
            .find(|execution| execution.agent.is_some())
            .unwrap();
        Agent {
            space: chat.id,
            execution: execution.id,
        }
    }

    /// A thread the user opens with the parent.
    async fn thread(&self, name: &str) -> Thread {
        let parent = SpaceSelector::id(&self.parent.space);
        (self.manager.coordination.open_thread(
            &*self.manager,
            None,
            name.into(),
            Some(ThreadKind::Question),
            Some(&parent),
            vec![],
        ))
        .await
        .unwrap()
    }

    /// Mark the parent's agent's turn as posting in `thread`.
    async fn mark_post(&self, thread: &Thread) -> ChatMarked {
        let (agent, selector) = (self.parent.caller(), SpaceSelector::id(&self.parent.space));
        let number = ThreadNumber::Full(thread.number);
        (self.manager.mark_chat_post(Some(&agent), &selector, number))
            .await
            .unwrap()
    }

    /// The texts of the posts `thread` has from the chat.
    async fn chat_posts(&self, thread: &Thread) -> Vec<String> {
        let number = ThreadNumber::Full(thread.number);
        let thread = self
            .manager
            .coordination
            .thread(&*self.manager, None, number)
            .await
            .unwrap();
        (thread.posts.into_iter())
            .filter(|post| post.in_chat)
            .map(|post| post.post.text)
            .collect()
    }

    /// The user's unread alerts.
    async fn users(&self) -> Vec<Alert> {
        self.manager.notifications.list(true, 50).await.unwrap()
    }

    /// The unread alerts of `agent`'s space.
    async fn unread(&self, agent: &Agent) -> Vec<Alert> {
        let unread = self
            .manager
            .agents
            .unread_messages(&agent.space)
            .await
            .unwrap();
        unread.alerts
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
    tree.parent.end_turn(&tree.manager, "Redis").await;
    assert!(tree.chat_posts(&database).await.is_empty());
    assert_eq!(tree.chat_posts(&cache).await, ["Redis"]);
}

#[tokio::test]
async fn a_marked_alert_goes_up_as_one_line_with_the_files_as_the_turn_ends() {
    let tree = tree().await;
    let file = tree.root.path().join("report.txt");
    std::fs::write(&file, "draft").unwrap();
    let marked = tree
        .parent
        .mark_alert(&tree.manager, AlertTarget::Up, vec![file.clone().into()])
        .await;
    // The parent has no coordinator, so the alert is the user's.
    assert_eq!(
        marked.mark,
        ChatMark::Alert {
            to: MarkedRecipients::User
        }
    );
    assert!(tree.users().await.is_empty());
    // The file goes as it is when the turn ends.
    std::fs::write(&file, "final").unwrap();
    (tree.parent)
        .end_turn(&tree.manager, "\u{1b}[1mDone\u{1b}[0m:\n\n- all tests pass")
        .await;
    // The mark lasts one turn.
    tree.parent.end_turn(&tree.manager, "Still here.").await;
    let users = tree.users().await;
    let [alert] = users.as_slice() else {
        panic!("{users:?}");
    };
    // Control characters go, as a message to the user takes none.
    assert_eq!(alert.message, "[1mDone [0m: - all tests pass");
    let attachments = alert.attachments();
    let [copy] = attachments.as_slice() else {
        panic!("{alert:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "final");
}

#[tokio::test]
async fn a_chat_s_marked_alert_reaches_its_coordinator() {
    let tree = tree().await;
    let helper = tree.chat_below().await;
    let marked = helper
        .mark_alert(&tree.manager, AlertTarget::Up, Vec::new())
        .await;
    let ChatMark::Alert {
        to: MarkedRecipients::Coordinator { space },
    } = marked.mark
    else {
        panic!("{marked:?}");
    };
    assert_eq!(space.id, tree.parent.space);
    helper.end_turn(&tree.manager, "Found the leak").await;
    let messages: Vec<String> = (tree.unread(&tree.parent).await.into_iter())
        .map(|alert| alert.message)
        .collect();
    assert_eq!(messages, ["Found the leak"]);
}

#[tokio::test]
async fn a_marked_alert_goes_down_in_lines_and_replaces_a_post_mark() {
    let tree = tree().await;
    let thread = tree.thread("Database?").await;
    tree.mark_post(&thread).await;
    let to = AlertTarget::Space {
        space: SpaceSelector::id(&tree.child.space),
    };
    let marked = tree.parent.mark_alert(&tree.manager, to, Vec::new()).await;
    assert_eq!(
        marked.replaced,
        Some(ChatMark::Post {
            thread: thread.number
        })
    );
    tree.parent
        .end_turn(&tree.manager, "Rebase onto main:\n\n\tgit rebase main")
        .await;
    assert!(tree.chat_posts(&thread).await.is_empty());
    let messages: Vec<String> = (tree.unread(&tree.child).await.into_iter())
        .map(|alert| alert.message)
        .collect();
    assert_eq!(messages, ["Rebase onto main:\n\n\tgit rebase main"]);
}

#[tokio::test]
async fn only_the_agent_of_a_space_marks_its_turn() {
    let tree = tree().await;
    let user = tree
        .manager
        .mark_chat_alert(None, AlertTarget::Up, Vec::new());
    assert!(user.await.is_err());
    // A process of the space that runs no agent.
    let command = caller_in(&SpaceSelector::id(&tree.parent.space));
    let marked = (tree.manager).mark_chat_alert(Some(&command), AlertTarget::Up, Vec::new());
    assert!(marked.await.is_err());
}

impl Agent {
    /// The user submits `prompt` in the agent's chat.
    async fn prompted(&self, manager: &Manager, prompt: &str) {
        let selector = SpaceSelector::id(&self.space);
        (manager.reply_in_chat(&selector, Some(&self.execution), Some(prompt.into())))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_prompt_during_a_marked_turn_drops_its_alert_and_tells_the_agent() {
    let tree = tree().await;
    (tree.parent)
        .mark_alert(&tree.manager, AlertTarget::Up, Vec::new())
        .await;
    tree.parent
        .prompted(&tree.manager, "Wait, one more thing")
        .await;
    tree.parent.end_turn(&tree.manager, "Sure").await;
    assert!(tree.users().await.is_empty());
    let kinds: Vec<AlertKind> = (tree.unread(&tree.parent).await.into_iter())
        .map(|alert| alert.kind)
        .collect();
    assert_eq!(kinds, [AlertKind::ChatMessageUnsent]);
}

#[tokio::test]
async fn a_prompt_marks_read_the_alerts_its_chat_sent_the_user() {
    let tree = tree().await;
    let parent = tree.parent.caller();
    let plain = tree.manager.coordination.send_alert(
        &*tree.manager,
        Some(&parent),
        AlertTarget::Up,
        "Plain".into(),
        Vec::new(),
    );
    plain.await.unwrap();
    for to in [
        AlertTarget::Up,
        AlertTarget::Space {
            space: SpaceSelector::id(&tree.child.space),
        },
    ] {
        tree.parent.mark_alert(&tree.manager, to, Vec::new()).await;
        tree.parent.end_turn(&tree.manager, "From the chat").await;
    }
    assert_eq!(tree.users().await.len(), 2);
    tree.parent.prompted(&tree.manager, "Thanks").await;
    // The alert sent with `send` and the child's stay unread.
    let users: Vec<String> = (tree.users().await.into_iter())
        .map(|alert| alert.message)
        .collect();
    assert_eq!(users, ["Plain"]);
    assert_eq!(tree.unread(&tree.child).await.len(), 1);
}

#[tokio::test]
async fn a_marked_alert_that_fails_as_the_turn_ends_is_told_to_the_agent() {
    let tree = tree().await;
    let file = tree.root.path().join("report.txt");
    std::fs::write(&file, "draft").unwrap();
    (tree.parent)
        .mark_alert(&tree.manager, AlertTarget::Up, vec![file.clone().into()])
        .await;
    std::fs::remove_file(&file).unwrap();
    tree.parent.end_turn(&tree.manager, "Done").await;
    assert!(tree.users().await.is_empty());
    let unsent: Vec<String> = (tree.unread(&tree.parent).await.into_iter())
        .filter(|alert| alert.kind == AlertKind::ChatMessageUnsent)
        .map(|alert| alert.message)
        .collect();
    let [notice] = unsent.as_slice() else {
        panic!("{unsent:?}");
    };
    assert!(notice.contains("report.txt"), "{notice}");
}

#[tokio::test]
async fn a_marked_alert_reaches_a_space_below_on_another_machine_through_the_hub() {
    let tree = tree().await;
    let manager = &tree.manager;
    away_on_offline_machine(manager, Some(&tree.parent.space)).await;
    let to = AlertTarget::Space {
        space: SpaceSelector::Number(90.into()),
    };
    let marked = tree.parent.mark_alert(manager, to, Vec::new()).await;
    let ChatMark::Alert {
        to: MarkedRecipients::Space { space, machine },
    } = marked.mark
    else {
        panic!("{marked:?}");
    };
    assert_eq!(
        (space.id.as_str(), machine.as_deref()),
        ("away", Some("desk"))
    );
    tree.parent.end_turn(manager, "Rebase onto main").await;
    let held = held_calls(manager).await;
    let [call] = held.as_slice() else {
        panic!("the hub holds {held:?}");
    };
    assert!(
        call.contains("send_alert") && call.contains("Rebase onto main"),
        "{call}"
    );
}

#[tokio::test]
async fn a_marked_alert_to_another_machine_takes_no_files() {
    let tree = tree().await;
    let manager = &tree.manager;
    away_on_offline_machine(manager, Some(&tree.parent.space)).await;
    let file = tree.root.path().join("report.txt");
    std::fs::write(&file, "draft").unwrap();
    let to = AlertTarget::Space {
        space: SpaceSelector::Number(90.into()),
    };
    let caller = tree.parent.caller();
    let marked = manager.mark_chat_alert(Some(&caller), to, vec![file.into()]);
    let error = marked.await.unwrap_err().to_string();
    assert!(error.contains("machine desk"), "{error}");
}
