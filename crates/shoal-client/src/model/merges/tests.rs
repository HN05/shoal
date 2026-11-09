use super::*;

#[test]
fn merge_preview_names_its_direction_push_warning_and_open_prs() {
    let mut preview = MergePreview {
        workspace_id: "id".into(),
        branch: "fix-login".into(),
        head: "abc".into(),
        default_branch: "main".into(),
        default_commit: "def".into(),
        commits: 1,
        push_to: None,
        unpushed: 0,
        warning: None,
        open_prs: Vec::new(),
    };
    let description = preview.description();
    assert!(description.contains("(1 commit)"), "{description}");
    assert!(!description.contains("push"), "{description}");
    preview.commits = 3;
    preview.push_to = Some(MergePush {
        remote: "origin".into(),
        reference: "refs/heads/main".into(),
    });
    preview.warning = Some(MergeWarning {
        subject: "fix-login".into(),
        target: "main".into(),
        files: vec!["version".into()],
        url: None,
    });
    preview.open_prs = vec!["https://forge/pulls/12".into()];
    let description = preview.description();
    for part in [
        "(3 commits)",
        "push main to origin.",
        ": version.",
        "https://forge/pulls/12",
    ] {
        assert!(description.contains(part), "{part:?} in {description}");
    }
    assert_eq!(preview.reason(), description.replace('\n', " "));
    preview.warning = None;
    preview.open_prs.clear();
    preview.unpushed = 2;
    let description = preview.description();
    assert!(description.contains("2 commits"), "{description}");
    assert!(description.contains("origin/main"), "{description}");
    preview.unpushed = 0;
    preview.push_to.as_mut().unwrap().reference = "refs/heads/release".into();
    assert!(
        preview
            .description()
            .ends_with("push main to origin/release.")
    );
    preview.branch = "b".repeat(300);
    assert_eq!(preview.reason().len(), crate::validate::MAX_REASON_LEN);
    assert!(preview.reason().ends_with('…'));
}
