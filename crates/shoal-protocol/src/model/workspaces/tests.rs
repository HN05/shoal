use super::*;
use crate::model::{Space, SpacePart};

/// A workspace's JSON holds its branch or its starting commit, never both
/// nor a null branch, and reads back as it was.
#[test]
fn a_workspace_serializes_its_head_as_one_of_branch_and_at() {
    let at = StartingCommit {
        commit: "84d4405".into(),
        made_for: Some(MadeFor::Ref("main".into())),
    };
    for head in [Head::Branch("fix-x".into()), Head::Commit(at)] {
        let workspace = Workspace::new_record(
            "repo".into(),
            "work".into(),
            "/work".into(),
            head.clone(),
            WorkspaceState::Ready,
        );
        let json = serde_json::to_value(Space::<SpacePart>::from(workspace)).unwrap();
        let keys = ["branch", "at"].map(|key| json.get(key).is_some());
        assert_eq!(
            keys,
            [head.branch().is_some(), head.at().is_some()],
            "{json}"
        );
        let read: Space = serde_json::from_value(json).unwrap();
        assert_eq!(
            read.into_workspace().ok().map(|read| read.part.head),
            Some(head)
        );
    }
}
