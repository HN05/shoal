use std::path::PathBuf;

use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::files::{
    AttachedFile, AttachedTo, Attacher, FileSource, KeptCopy, SessionCopy,
};
use shoal_protocol::integrations::IntegrationKind;

use super::{Listed, Present, changed_since_copied, listing};

/// The handoff copy `id` of `file` of claude's `session`, `bytes` long.
fn copy(id: i64, session: &str, file: &str, bytes: u64) -> AttachedFile {
    AttachedFile {
        id,
        space: AttachedTo {
            id: "space".into(),
            name: "space".into(),
            kind: crate::model::SpaceKind::Chat,
        },
        source: FileSource::Handoff,
        name: file.into(),
        title: None,
        copy: KeptCopy::Here {
            path: format!("/files/{id}/{file}").into(),
        },
        bytes,
        created_at: crate::time::UnixTime::from_seconds(0),
        attached_by: Attacher::User,
        session: Some(SessionCopy {
            agent: "claude".into(),
            session: session.into(),
            integration: IntegrationKind::Claude,
            file: file.into(),
            transcript: !file.contains('/'),
        }),
        expires_at: None,
    }
}

fn present(file: &str, bytes: u64) -> Present {
    Present {
        relative: file.into(),
        path: PathBuf::from("/sessions").join(file),
        bytes,
        transcript: !file.contains('/'),
    }
}

fn session(id: &str) -> AgentSessionId {
    id.into()
}

#[test]
fn a_session_is_copied_again_only_once_its_files_changed() {
    let earlier = [
        copy(1, "s1", "s1.jsonl", 10),
        copy(2, "s1", "agents/a.jsonl", 5),
    ];
    let same = [present("s1.jsonl", 10), present("agents/a.jsonl", 5)];
    assert!(!changed_since_copied(&session("s1"), &same, &earlier));
    let grown = [present("s1.jsonl", 12), present("agents/a.jsonl", 5)];
    assert!(changed_since_copied(&session("s1"), &grown, &earlier));
    let more = [
        present("s1.jsonl", 10),
        present("agents/a.jsonl", 5),
        present("agents/b.jsonl", 1),
    ];
    assert!(changed_since_copied(&session("s1"), &more, &earlier));
    assert!(changed_since_copied(&session("s2"), &same, &earlier));
}

#[test]
fn a_session_copied_again_compares_with_its_newest_copy() {
    let earlier = [copy(1, "s1", "s1.jsonl", 10), copy(3, "s1", "s1.jsonl", 12)];
    assert!(!changed_since_copied(
        &session("s1"),
        &[present("s1.jsonl", 12)],
        &earlier
    ));
}

#[test]
fn sessions_list_in_the_order_first_copied_with_their_newest_transcript() {
    let mut added = copy(4, "s2", "notes.md", 1);
    added.source = FileSource::Added;
    added.session = None;
    let attached = [
        copy(1, "s1", "s1.jsonl", 10),
        copy(2, "s1", "agents/a.jsonl", 5),
        copy(3, "s2", "s2.jsonl", 7),
        added,
        copy(5, "s1", "s1.jsonl", 12),
    ];
    let missing = |id: &str| Listed {
        agent: "codex".into(),
        session: session(id),
        integration: IntegrationKind::Codex,
        transcript: None,
        others: 0,
        missing: Some("its files are not on this machine".into()),
    };
    let listed = listing(&attached, vec![missing("s3"), missing("s2")]);
    let summary: Vec<_> = (listed.iter())
        .map(|listed| {
            let transcript = listed
                .transcript
                .as_ref()
                .map(|path| path.display().to_string());
            (
                listed.session.as_str().to_owned(),
                transcript,
                listed.others,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("s1".to_owned(), Some("/files/5/s1.jsonl".to_owned()), 1),
            ("s2".to_owned(), Some("/files/3/s2.jsonl".to_owned()), 0),
            ("s3".to_owned(), None, 0),
        ]
    );
}
