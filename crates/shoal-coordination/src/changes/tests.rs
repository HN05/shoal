use std::fs;

use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeNumber, ToolChange},
    model::Workspace,
    protocol::ChangeFilter,
    space_names::SpaceSelector,
    threads::{OutcomeKind, Resolving, Thread, ThreadKind, ThreadNumber, ThreadOpening},
};

use crate::{scope::Caller, stand_in};

/// A workspace in a Git worktree whose agent's changes Shoal records.
struct Fixture {
    inner: stand_in::Fixture,
    workspace: Workspace,
    agent: Caller,
}

async fn fixture() -> Fixture {
    let inner = stand_in::Fixture::new().await;
    shoal_core::test_support::repository(inner.root.path(), "work");
    let workspace = inner.workspace("work", None).await;
    let agent = Caller::of_space(workspace.id.clone());
    Fixture {
        inner,
        workspace,
        agent,
    }
}

impl Fixture {
    fn write(&self, path: &str, contents: &str) {
        fs::write(self.workspace.path.join(path), contents).unwrap();
    }

    async fn report(&self, change: ToolChange) -> Vec<Change> {
        let coordination = &self.inner.coordination;
        let recorded = coordination.record_tool_change(&*self.inner.daemon, &self.agent, change);
        recorded.await.unwrap()
    }

    /// A command that writes `contents` to `path`.
    async fn command(&self, call: &str, path: &str, contents: &str) -> Vec<Change> {
        self.report(ToolChange::Starts { call: call.into() }).await;
        self.write(path, contents);
        self.report(ToolChange::Ends { call: call.into() }).await
    }

    async fn thread(&self, name: &str) -> Thread {
        let opened = self.inner.coordination.open_thread(
            &*self.inner.daemon,
            Some(&self.agent),
            ThreadOpening::named(name),
            Some(ThreadKind::Request),
            None,
        );
        opened.await.unwrap()
    }

    async fn switch(&self, thread: Option<&Thread>) {
        let number = thread.map(|thread| ThreadNumber::Full(thread.number));
        let own = SpaceSelector::id(&self.workspace.id);
        let switched = (self.inner.coordination).switch_thread(
            &*self.inner.daemon,
            Some(&self.agent),
            &own,
            number,
        );
        switched.await.unwrap();
    }

    /// The numbers of the changes of `thread`.
    async fn of_thread(&self, thread: &Thread) -> Vec<u32> {
        let filter = ChangeFilter {
            thread: Some(ThreadNumber::Full(thread.number)),
            ..ChangeFilter::default()
        };
        let coordination = &self.inner.coordination;
        let changes = coordination.changes(&*self.inner.daemon, Some(&self.agent), filter);
        let changes = changes.await.unwrap();
        changes.iter().map(|change| change.number).collect()
    }
}

fn own(numbers: &[u32]) -> Vec<ChangeNumber> {
    numbers.iter().copied().map(ChangeNumber::Own).collect()
}

#[tokio::test]
async fn changes_go_to_the_thread_switched_to_and_move_as_the_agent_says() {
    let fixture = fixture().await;
    let (agent, coordination) = (Some(&fixture.agent), &fixture.inner.coordination);
    let daemon = &*fixture.inner.daemon;
    let first = fixture.thread("Parse it").await;
    let second = fixture.thread("Lex it").await;
    fixture.switch(Some(&first)).await;
    fixture.write("lib.rs", "a\n");
    fixture.command("1", "lib.rs", "a\nb\n").await;
    // Someone else's change between tool calls goes to no thread.
    fixture.write("notes.md", "notes\n");
    fixture.command("2", "lib.rs", "a\nb\nc\n").await;
    assert_eq!(fixture.of_thread(&first).await, [1, 3]);

    let (first_number, second_number) = (
        ThreadNumber::Full(first.number),
        ThreadNumber::Full(second.number),
    );
    let (two, three) = (own(&[2]), own(&[3]));
    let claimed = coordination.claim_changes(daemon, agent, None, &two, Some(second_number));
    assert_eq!(claimed.await.unwrap()[0].author, ChangeAuthor::Agent);
    let added = coordination.add_changes(daemon, agent, second_number, None, &three);
    added.await.unwrap();
    assert_eq!(fixture.of_thread(&first).await, [1]);
    assert_eq!(fixture.of_thread(&second).await, [2, 3]);
    let removed = coordination.remove_changes(daemon, agent, first_number, None, &three);
    assert!(
        removed.await.is_err(),
        "change 3 is not in the first thread"
    );
    let unclaimed = coordination
        .unclaim_changes(daemon, agent, None, &three)
        .await
        .unwrap();
    assert_eq!(
        (unclaimed[0].author, unclaimed[0].thread),
        (ChangeAuthor::Others, None)
    );

    // Resolving the thread switched to leaves the agent in none.
    let resolved = coordination.resolve_thread(
        daemon,
        agent,
        first_number,
        Resolving {
            outcome: Some(OutcomeKind::Withdrawn),
            ..Resolving::default()
        },
    );
    resolved.await.unwrap();
    let own_space = SpaceSelector::id(&fixture.workspace.id);
    assert_eq!(
        coordination
            .current_thread(daemon, &own_space)
            .await
            .unwrap(),
        None
    );
    let switched = coordination.switch_thread(daemon, agent, &own_space, Some(first_number));
    assert!(
        switched.await.is_err(),
        "a resolved thread takes no changes"
    );
}

#[tokio::test]
async fn a_command_s_change_of_a_file_the_user_edited_meanwhile_is_mixed() {
    let fixture = fixture().await;
    fixture.write("lib.rs", "a\n");
    fixture.write("main.rs", "fn main() {}\n");
    fixture
        .report(ToolChange::Starts { call: "1".into() })
        .await;
    let edited = vec![fixture.workspace.path.join("lib.rs")];
    let inner = &fixture.inner;
    let reported = inner
        .coordination
        .report_edits(&*inner.daemon, None, edited);
    reported.await.unwrap();
    fixture.write("lib.rs", "a\nb\n");
    fixture.write("main.rs", "fn main() { run() }\n");
    let mixed = fixture.report(ToolChange::Ends { call: "1".into() }).await;
    let paths: Vec<&str> = mixed
        .iter()
        .map(|change| change.latest.path.as_str())
        .collect();
    assert_eq!(paths, ["lib.rs"]);
}
