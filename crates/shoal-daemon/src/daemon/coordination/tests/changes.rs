use std::{fs, sync::Arc};

use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeKind, ChangeNumber, Hunk, ToolChange},
    space_names::SpaceSelector,
    threads::{NewPost, OutcomeKind, Resolving, Thread, ThreadKind, ThreadNumber, ThreadStatus},
};

use crate::{
    daemon::{
        manager::Manager,
        scope::Caller,
        test_support::{caller_in, manager, repository},
    },
    model::Workspace,
    protocol::ChangeFilter,
};
use shoal_coordination::changes::records;

/// A workspace whose agent's changes Shoal records.
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) manager: Arc<Manager>,
    pub(crate) workspace: Workspace,
    pub(crate) agent: Caller,
}

pub(crate) async fn fixture() -> Fixture {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "work".into(), None, None, None)
        .await
        .unwrap();
    let agent = caller_in(&SpaceSelector::id(&workspace.id));
    Fixture {
        _root: root,
        manager,
        workspace,
        agent,
    }
}

impl Fixture {
    pub(crate) fn write(&self, path: &str, contents: &str) {
        fs::write(self.workspace.path.join(path), contents).unwrap();
    }

    pub(crate) async fn report(&self, change: ToolChange) -> Vec<Change> {
        let recorded =
            self.manager
                .coordination
                .record_tool_change(&*self.manager, &self.agent, change);
        recorded.await.unwrap()
    }

    /// An edit tool's write of `contents` to `path`, whose hunks it reports.
    pub(crate) async fn edit(&self, path: &str, before: &str, contents: &str) -> Vec<Change> {
        self.write(path, contents);
        let hunks = shoal_protocol::changes::diff(before, contents);
        let path = self.workspace.path.join(path);
        self.report(ToolChange::Edited { path, hunks }).await
    }

    /// A command that writes `contents` to `path`.
    pub(crate) async fn command(&self, call: &str, path: &str, contents: &str) -> Vec<Change> {
        self.report(ToolChange::Starts { call: call.into() }).await;
        self.write(path, contents);
        self.report(ToolChange::Ends { call: call.into() }).await
    }

    /// Every change recorded, as `number author kind path:lines`.
    pub(crate) async fn recorded(&self) -> Vec<String> {
        let changes = (self.manager.store)
            .read(|tx| records::query(tx, "", [], records::change))
            .await
            .unwrap();
        (changes.iter())
            .map(|change| {
                let (number, author, kind) = (change.number, change.author, change.kind);
                format!("{number} {author} {kind} {}", change.latest)
            })
            .collect()
    }
}

#[tokio::test]
async fn an_edit_tool_s_hunks_are_its_agent_s_changes_numbered_in_its_space() {
    let fixture = fixture().await;
    let before = "a\nb\nc\nd\n";
    fixture.write("parse.rs", before);
    let mixed = fixture.edit("parse.rs", before, "a\nB\nc\nd\ne\n").await;
    assert!(mixed.is_empty(), "{mixed:?}");
    // Someone else changed the line the tool reports it wrote.
    let hunks = vec![Hunk::created("written\n")];
    fixture.write("other.rs", "rewritten\n");
    let path = fixture.workspace.path.join("other.rs");
    let mixed = fixture.report(ToolChange::Edited { path, hunks }).await;
    assert_eq!(mixed[0].number, 3);
    assert_eq!(
        fixture.recorded().await,
        [
            "1 agent text parse.rs:2",
            "2 agent text parse.rs:5",
            "3 mixed text other.rs:1",
        ]
    );
}

#[tokio::test]
async fn a_command_s_change_is_its_agent_s_and_one_between_tool_calls_someone_else_s() {
    let fixture = fixture().await;
    fixture.write("lib.rs", "one\n");
    fixture.command("1", "lib.rs", "one\ntwo\n").await;
    // An edit tool's edit between commands stays the agent's.
    fixture
        .edit("lib.rs", "one\ntwo\n", "zero\none\ntwo\n")
        .await;
    fixture.write("notes.md", "the user's\n");
    fixture
        .command("2", "lib.rs", "zero\none\ntwo\nthree\n")
        .await;
    assert_eq!(
        fixture.recorded().await,
        [
            "1 agent text lib.rs:2",
            "2 agent text lib.rs:1",
            "3 others text notes.md:1",
            "4 agent text lib.rs:4",
        ]
    );
}

#[tokio::test]
async fn a_command_after_its_base_was_pruned_takes_a_new_one() {
    let fixture = fixture().await;
    fixture.write("lib.rs", "a\n");
    fixture.command("1", "lib.rs", "a\nb\n").await;
    // A prune deletes the snapshot, which no ref keeps.
    let id = fixture.workspace.id.clone();
    let gone = "5b825dc652cb6eb9a060e55bf8d69288fbee5905";
    (fixture.manager.store)
        .write(move |tx| records::set_base(tx, &id, gone))
        .await
        .unwrap();
    fixture.edit("lib.rs", "a\nb\n", "a\nb\nc\n").await;
    fixture.command("2", "lib.rs", "a\nb\nc\nd\n").await;
    fixture.command("3", "lib.rs", "a\nb\nc\nd\ne\n").await;
    assert_eq!(
        fixture.recorded().await,
        [
            "1 agent text lib.rs:2",
            "2 agent text lib.rs:3",
            "3 agent text lib.rs:4",
            "4 agent text lib.rs:5",
        ]
    );
}

#[tokio::test]
async fn an_edit_to_a_file_git_ignores_is_no_change() {
    let fixture = fixture().await;
    fixture.write(".gitignore", ".env\n");
    fixture.command("1", "lib.rs", "a\n").await;
    fixture.edit(".env", "", "TOKEN=1\n").await;
    fixture.command("2", "lib.rs", "a\nb\n").await;
    assert_eq!(
        fixture.recorded().await,
        ["1 agent text lib.rs:1", "2 agent text lib.rs:2"]
    );
}

#[tokio::test]
async fn the_user_s_edit_before_an_edit_tool_s_is_someone_else_s() {
    let fixture = fixture().await;
    fixture.command("1", "lib.rs", "a\nb\n").await;
    // The user edits the file, then the agent's edit tool edits it too.
    fixture.write("lib.rs", "a\nb\nuser\n");
    fixture
        .edit("lib.rs", "a\nb\nuser\n", "agent\na\nb\nuser\n")
        .await;
    fixture
        .command("2", "lib.rs", "agent\na\nb\nuser\nmore\n")
        .await;
    assert_eq!(
        fixture.recorded().await,
        [
            "1 agent text lib.rs:1-2",
            "2 others text lib.rs:3",
            "3 agent text lib.rs:1",
            "4 agent text lib.rs:5",
        ]
    );
}

#[tokio::test]
async fn what_a_running_command_wrote_before_an_edit_stays_the_agent_s() {
    let fixture = fixture().await;
    fixture.command("1", "lib.rs", "a\n").await;
    fixture
        .report(ToolChange::Starts { call: "2".into() })
        .await;
    // The command writes the file, and an edit tool edits it meanwhile.
    fixture.write("lib.rs", "a\ncommand\n");
    fixture
        .edit("lib.rs", "a\ncommand\n", "edit\na\ncommand\n")
        .await;
    fixture.report(ToolChange::Ends { call: "2".into() }).await;
    assert_eq!(
        fixture.recorded().await,
        [
            "1 agent text lib.rs:1",
            "2 agent text lib.rs:2",
            "3 agent text lib.rs:1",
        ]
    );
}

#[tokio::test]
async fn a_file_without_hunks_is_recorded_with_its_kind_and_size() {
    let fixture = fixture().await;
    fixture.write(".gitattributes", "*.lock linguist-generated\n");
    fixture
        .report(ToolChange::Starts { call: "1".into() })
        .await;
    fixture.write("Cargo.lock", "version = 4\n");
    fs::write(fixture.workspace.path.join("logo.png"), b"\x89PNG\0").unwrap();
    fixture.report(ToolChange::Ends { call: "1".into() }).await;
    let changes = (fixture.manager.store)
        .read(|tx| records::query(tx, "", [], records::change))
        .await
        .unwrap();
    let files: Vec<(&str, ChangeKind, Option<u64>, ChangeAuthor)> = (changes.iter())
        .map(|change| {
            (
                change.latest.path.as_str(),
                change.kind,
                change.size,
                change.author,
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            (
                "Cargo.lock",
                ChangeKind::Generated,
                Some(12),
                ChangeAuthor::Agent
            ),
            ("logo.png", ChangeKind::Binary, Some(5), ChangeAuthor::Agent),
        ]
    );
}

impl Fixture {
    /// A thread the workspace's agent opens with the user.
    pub(crate) async fn thread(&self, name: &str) -> Thread {
        let opened = (self.manager).coordination.open_thread(
            &*self.manager,
            Some(&self.agent),
            name.into(),
            Some(ThreadKind::Request),
            None,
            Vec::new(),
        );
        opened.await.unwrap()
    }

    pub(crate) async fn switch(&self, thread: Option<&Thread>) {
        let number = thread.map(|thread| ThreadNumber::Full(thread.number));
        let own = SpaceSelector::id(&self.workspace.id);
        let switched = self.manager.coordination.switch_thread(
            &*self.manager,
            Some(&self.agent),
            &own,
            number,
        );
        switched.await.unwrap();
    }

    /// The numbers of the changes of `thread`.
    pub(crate) async fn of_thread(&self, thread: &Thread) -> Vec<u32> {
        let filter = ChangeFilter {
            thread: Some(ThreadNumber::Full(thread.number)),
            ..ChangeFilter::default()
        };
        let changes = self
            .manager
            .coordination
            .changes(&*self.manager, Some(&self.agent), filter)
            .await
            .unwrap();
        changes.iter().map(|change| change.number).collect()
    }
}

fn own(numbers: &[u32]) -> Vec<ChangeNumber> {
    numbers.iter().copied().map(ChangeNumber::Own).collect()
}

#[tokio::test]
async fn changes_go_to_the_thread_switched_to_and_move_as_the_agent_says() {
    let fixture = fixture().await;
    let (agent, manager) = (Some(&fixture.agent), &fixture.manager);
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
    let claimed =
        manager
            .coordination
            .claim_changes(&**manager, agent, None, &two, Some(second_number));
    assert_eq!(claimed.await.unwrap()[0].author, ChangeAuthor::Agent);
    let added = manager
        .coordination
        .add_changes(&**manager, agent, second_number, None, &three);
    added.await.unwrap();
    assert_eq!(fixture.of_thread(&first).await, [1]);
    assert_eq!(fixture.of_thread(&second).await, [2, 3]);
    let removed =
        manager
            .coordination
            .remove_changes(&**manager, agent, first_number, None, &three);
    assert!(
        removed.await.is_err(),
        "change 3 is not in the first thread"
    );
    let unclaimed = manager
        .coordination
        .unclaim_changes(&**manager, agent, None, &three)
        .await
        .unwrap();
    assert_eq!(
        (unclaimed[0].author, unclaimed[0].thread),
        (ChangeAuthor::Others, None)
    );

    // Resolving the thread switched to leaves the agent in none.
    let resolved = manager.coordination.resolve_thread(
        &**manager,
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
        manager
            .coordination
            .current_thread(&**manager, &own_space)
            .await
            .unwrap(),
        None
    );
    let switched =
        manager
            .coordination
            .switch_thread(&**manager, agent, &own_space, Some(first_number));
    assert!(
        switched.await.is_err(),
        "a resolved thread takes no changes"
    );
}

#[tokio::test]
async fn a_space_that_forwards_a_thread_works_for_it_no_longer() {
    let fixture = fixture().await;
    let manager = &fixture.manager;
    let parent = SpaceSelector::id(&fixture.workspace.id);
    let child = (manager.create_child(&parent, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let (to, child_agent) = (
        SpaceSelector::id(&child.id),
        caller_in(&SpaceSelector::id(&child.id)),
    );
    let opened = manager.coordination.open_thread(
        &**manager,
        Some(&fixture.agent),
        "Lex it".into(),
        Some(ThreadKind::Request),
        Some(&to),
        Vec::new(),
    );
    let number = ThreadNumber::Full(opened.await.unwrap().number);
    (manager
        .coordination
        .switch_thread(&**manager, Some(&child_agent), &to, Some(number)))
    .await
    .unwrap();
    (manager
        .coordination
        .forward_thread(&**manager, Some(&child_agent), number, None, None))
    .await
    .unwrap();
    assert_eq!(
        manager
            .coordination
            .current_thread(&**manager, &to)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_thread_shows_its_changes_and_whether_its_holder_works_for_it() {
    let fixture = fixture().await;
    let (manager, agent) = (&fixture.manager, Some(&fixture.agent));
    let to = SpaceSelector::id(&fixture.workspace.id);
    let opened = manager.coordination.open_thread(
        &**manager,
        None,
        "Fix the parser".into(),
        Some(ThreadKind::Request),
        Some(&to),
        Vec::new(),
    );
    let thread = opened.await.unwrap();
    let number = ThreadNumber::Full(thread.number);
    let asked = NewPost {
        text: "Please".into(),
        ..NewPost::default()
    };
    (manager.coordination.post_to_thread(
        &**manager,
        None,
        number,
        asked,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap();
    let delivered = manager
        .agents
        .agent_messages(&**manager, &to)
        .await
        .unwrap();
    let switch = format!("shoal thread switch {}", thread.number);
    assert!(delivered[0].message.contains(&switch), "{delivered:?}");

    fixture.switch(Some(&thread)).await;
    fixture.write("lib.rs", "a\n");
    fixture.command("1", "lib.rs", "a\nb\nc\n").await;
    let done = NewPost {
        text: "Done".into(),
        ..NewPost::default()
    };
    (manager.coordination.post_to_thread(
        &**manager,
        agent,
        number,
        done,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap();
    let shown = manager
        .coordination
        .thread(&**manager, None, number)
        .await
        .unwrap();
    assert_eq!(shown.status(), ThreadStatus::Working);
    let entries: Vec<String> = (shown.changes.iter())
        .map(|entry| entry.numbers_text())
        .collect();
    assert_eq!(entries, ["1"]);
    let told = manager.notifications.list(true, 10).await.unwrap();
    let size = (told.iter()).find(|alert| alert.message.contains("Done"));
    assert!(
        size.unwrap().message.contains("1 file changed, +2 −0"),
        "{told:?}"
    );
}

#[tokio::test]
async fn a_change_s_lines_follow_its_code_and_its_version_stays_kept() {
    let fixture = fixture().await;
    fixture.write("lib.rs", "a\nb\n");
    fixture.edit("lib.rs", "a\nb\n", "a\nb\nc\n").await;
    // Lines added above it move it down, once the agent commits it too.
    fixture.write("lib.rs", "z\na\nb\nc\n");
    let lines = |changes: Vec<Change>| changes[0].latest.to_string();
    let filter = ChangeFilter::default;
    let changes = fixture
        .manager
        .coordination
        .changes(&*fixture.manager, Some(&fixture.agent), filter())
        .await
        .unwrap();
    assert_eq!(lines(changes), "lib.rs:4");
    let repository = fixture.workspace.repository_id.clone().unwrap();
    let repo = fixture
        .manager
        .repositories
        .repository(&repository)
        .await
        .unwrap();
    let reference = crate::git::code_versions::reference(fixture.workspace.id.as_str());
    let kept = crate::git::code_versions::kept(&repo.path, &reference)
        .await
        .unwrap();
    assert_eq!(
        kept.len(),
        2,
        "the version it made and the one it was followed to"
    );
}

#[tokio::test]
async fn a_change_in_no_thread_expires_as_checkpoints_do() {
    let fixture = fixture().await;
    let thread = fixture.thread("Keep it").await;
    fixture.write("lib.rs", "a\n");
    fixture.switch(Some(&thread)).await;
    fixture.edit("lib.rs", "a\n", "a\nb\n").await;
    fixture.switch(None).await;
    fixture.edit("lib.rs", "a\nb\n", "a\nb\nc\n").await;
    (fixture.manager.store)
        .write(|tx| Ok(tx.execute("UPDATE space_changes SET made_at=0", [])?))
        .await
        .unwrap();
    fixture.manager.expire_checkpoints().await.unwrap();
    assert_eq!(fixture.recorded().await, ["1 agent text lib.rs:2"]);
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
    fixture
        .manager
        .coordination
        .report_edits(&*fixture.manager, None, edited)
        .await
        .unwrap();
    fixture.write("lib.rs", "a\nb\n");
    fixture.write("main.rs", "fn main() { run() }\n");
    let mixed = fixture.report(ToolChange::Ends { call: "1".into() }).await;
    let paths: Vec<&str> = mixed
        .iter()
        .map(|change| change.latest.path.as_str())
        .collect();
    assert_eq!(paths, ["lib.rs"]);
}
