#[test]
fn a_launch_holds_each_commit_it_carried_once() {
    let commit = shoal_refs::CommitId::from("0123456789abcdef0123456789abcdef01234567");
    let files = shoal_refs::CommitId::from("89abcdef0123456789abcdef0123456789abcdef");
    let launch = |files: &shoal_refs::CommitId| shoal_protocol::launches::Launch {
        forwarded: Some(shoal_protocol::launches::Carried {
            origin: Some("https://forge.example/team/app".into()),
            commit: Some(commit.clone()),
            ..Default::default()
        }),
        carried_copy: Some(shoal_protocol::launches::CarriedCopy {
            files: files.clone(),
            name: "fix-copy".into(),
        }),
        ..Default::default()
    };
    let held = |files| crate::launches::carried_commits(&launch(files)).unwrap().1;
    assert_eq!(held(&files), [commit.clone(), files.clone()]);
    // A copy without unsaved files carried them as its base.
    assert_eq!(held(&commit), std::slice::from_ref(&commit));
}
