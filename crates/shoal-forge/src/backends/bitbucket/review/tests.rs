use super::*;
use crate::items::{
    item::{Item, ItemState},
    link::ItemKind,
};

#[test]
fn the_diff_runs_from_the_merge_base_to_the_head() -> Result<()> {
    let repo = ForgeRepo::parse("https://bitbucket.org/team/repo.git")?;
    let mut current = Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 3,
            url: String::new(),
            title: "Fix".into(),
            state: ItemState::Open,
            labels: Vec::new(),
            pr: None,
        },
        node_id: None,
        head_repository: None,
        body: String::new(),
        head_sha: Some("abc".into()),
        base_sha: Some("def".into()),
        conflict: None,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        replaced_labels: Vec::new(),
    };
    assert_eq!(
        diff_endpoint(&repo, &current)?,
        "repositories/team/repo/diff/abc..def?topic=true"
    );
    current.base_sha = None;
    assert!(diff_endpoint(&repo, &current).is_err());
    Ok(())
}
