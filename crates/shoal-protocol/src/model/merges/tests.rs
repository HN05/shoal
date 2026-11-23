use super::*;

#[test]
fn merge_preview_names_its_direction_push_and_warning() {
    let mut preview = MergePreview {
        workspace_id: "id".into(),
        branch: "fix-login".into(),
        head: "abc".into(),
        method: MergeMethod::Merge,
        message: None,
        default_branch: "main".into(),
        default_commit: "def".into(),
        commits: 1,
        push_to: None,
        unpushed: 0,
        warning: None,
        asks_before_push: false,
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
    let description = preview.description();
    for part in ["(3 commits)", "push main to origin.", ": version."] {
        assert!(description.contains(part), "{part:?} in {description}");
    }
    assert_eq!(preview.reason(), description.replace('\n', " "));
    preview.warning = None;
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
    preview.branch = "b".repeat(300).into();
    assert_eq!(preview.reason().len(), crate::validate::MAX_REASON_LEN);
    assert!(preview.reason().ends_with('…'));
}

#[test]
fn merge_preview_names_its_method_and_a_squash_s_subject() {
    let mut preview = MergePreview {
        workspace_id: "id".into(),
        branch: "fix-login".into(),
        head: "abc".into(),
        method: MergeMethod::Squash,
        message: Some("Fix login\n\nThe session expired early.".into()),
        default_branch: "main".into(),
        default_commit: "def".into(),
        commits: 2,
        push_to: None,
        unpushed: 0,
        warning: None,
        asks_before_push: false,
    };
    let description = preview.description();
    for part in ["Squash fix-login", "as \"Fix login\""] {
        assert!(description.contains(part), "{part:?} in {description}");
    }
    preview.method = MergeMethod::Rebase;
    preview.message = None;
    let description = preview.description();
    assert!(description.starts_with("Rebase fix-login"), "{description}");
}
