use super::*;
use crate::model::{ChatPart, LinkedItem, SpacePart, Workdir, Workspace};

const ISSUE: &str = "https://forge.example/team/repo/issues/7";

/// A workspace on branch `fix-login` that links issue 7.
fn workspace() -> Space {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "talk".into(),
        "/work/fix-login".into(),
        crate::model::Head::Branch("fix-login".into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.links.issues = vec![LinkedItem {
        url: ISSUE.parse().unwrap(),
        title: Some("Fix the login".into()),
    }];
    Space::from(workspace)
}

/// The space of [`workspace`] as a chat in the checkout at `/repos/repo`.
fn chat() -> Space {
    Space {
        workdir: Workdir::Checkout {
            path: "/repos/repo".into(),
        },
        part: SpacePart::Chat(ChatPart {
            agent: None,
            started_at: crate::time::UnixTime::now(),
            tmp_dir: None,
            completion: None,
        }),
        ..workspace()
    }
}

/// The last session of an agent whose space changed as `last` says.
fn last(change: impl FnOnce(&mut LastSession)) -> LastSession {
    let mut last: LastSession = serde_json::from_value(serde_json::json!({
        "execution": "e1",
        "agent": "claude",
        "session": "s1",
        "mode": "worker",
    }))
    .unwrap();
    change(&mut last);
    last
}

#[test]
fn a_session_names_how_its_space_changed() {
    let held = Copied {
        earlier_files: false,
        held_elsewhere: true,
    };
    assert_eq!(Change::of(&last(|_| {})), None);
    let moved = last(|last| last.moved = true);
    assert_eq!(Change::of(&moved), Some(Change::Moved));
    let copied = last(|last| last.copied = Some(held));
    assert_eq!(Change::of(&copied), Some(Change::Copied(held)));
    // A converted space is told so, whatever else it is.
    let converted = last(|last| {
        last.converted = true;
        last.copied = Some(held);
    });
    assert_eq!(Change::of(&converted), Some(Change::Converted));
    assert!(!Change::Moved.forks());
}

#[test]
fn a_moved_chat_is_told_it_moved() {
    let told = Change::Moved.told(&chat(), false, true, None).text;
    assert!(told.contains("moved this chat"), "{told}");
    let told = Change::Moved
        .told(&workspace(), false, true, Some("handoff"))
        .text;
    assert!(told.contains("moved this workspace"), "{told}");
    assert!(told.contains("Continue working"), "{told}");
    assert!(told.ends_with("\n\nhandoff"), "{told}");
}

#[test]
fn a_space_whose_session_ended_between_turns_is_not_told_to_continue() {
    let moved = Change::Moved.told(&workspace(), false, false, Some("handoff"));
    assert!(!moved.text.contains("Continue"), "{}", moved.text);
    assert!(moved.text.ends_with("\n\nhandoff"), "{}", moved.text);
    let copied = Change::Copied(Copied::default()).told(&chat(), true, false, None);
    assert!(!copied.text.contains("Continue"), "{}", copied.text);
}

#[test]
fn a_converted_space_is_told_its_new_place_with_its_issue() {
    let told = Change::Converted.told(&workspace(), true, true, Some("handoff"));
    let text = &told.text;
    assert!(
        text.contains("/work/fix-login") && text.contains("fix-login"),
        "{text}"
    );
    assert!(text.ends_with("\n\nhandoff"), "{text}");
    // Its mode's instructions name the issue it was linked to before.
    let issue = told.issue.expect("the linked issue's fields");
    assert_eq!(
        (issue.number, issue.title.as_str(), issue.url.as_str()),
        (7, "Fix the login", ISSUE)
    );
    let told = Change::Converted.told(&chat(), true, true, None);
    assert!(told.text.contains("/repos/repo"), "{}", told.text);
    assert_eq!(told.issue.map(|issue| issue.number), Some(7));
}

#[test]
fn a_copy_is_told_whether_it_forked_and_how_old_its_files_are() {
    let forked = Change::Copied(Copied::default()).told(&workspace(), true, true, None);
    assert!(
        forked.text.contains("a fork of its agent's"),
        "{}",
        forked.text
    );
    assert!(
        !forked.text.contains("earlier checkpoint"),
        "{}",
        forked.text
    );
    let earlier = Copied {
        earlier_files: true,
        held_elsewhere: false,
    };
    let earlier = Change::Copied(earlier)
        .told(&workspace(), false, true, None)
        .text;
    assert!(
        earlier.contains("session is not available here"),
        "{earlier}"
    );
    assert!(earlier.contains("earlier checkpoint"), "{earlier}");
    let held = Copied {
        earlier_files: false,
        held_elsewhere: true,
    };
    let restored = Change::Copied(held).told(&chat(), true, true, None).text;
    assert!(restored.contains("restored"), "{restored}");
}

#[test]
fn a_handoffs_new_tool_starts_anew_on_its_brief_whatever_else_the_space_is() {
    let brief = HandoffBrief {
        told: "Codex takes over".into(),
        note: None,
    };
    let handed_over = last(|last| {
        last.converted = true;
        last.handoff = Some(brief.clone());
    });
    let change = Change::of(&handed_over).expect("a handoff");
    assert_eq!(change, Change::HandedOver(brief));
    assert!(!change.forks());
    let told = change.told(&workspace(), false, true, Some("handoff"));
    assert_eq!(told.text, "Codex takes over\n\nhandoff");
    assert_eq!(told.issue.map(|issue| issue.number), Some(7));
}
