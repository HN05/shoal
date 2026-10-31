use super::*;

#[test]
fn urls_and_the_pr_flag_select_a_pr_and_forge_options_require_one() {
    let url = "https://forge.example/team/repo/pulls/7";
    for (workspace, pr) in [(Some(url), None), (None, Some("7"))] {
        let target = Target::new(
            workspace.map(String::from),
            pr.map(String::from),
            Some("repo".into()),
            Some(false),
        )
        .unwrap();
        assert!(matches!(
            target,
            Target::Pull {
                repository: Some(_),
                ..
            }
        ));
    }
    let issue = "https://forge.example/team/repo/issues/12";
    let refused = Target::new(Some(issue.into()), None, None, None)
        .err()
        .unwrap();
    assert!(refused.to_string().contains("shoal plan"), "{refused}");
    let branch = "https://forge.example/team/repo/src/branch/main";
    assert!(Target::new(Some(branch.into()), None, None, None).is_err());
    assert!(matches!(
        Target::new(Some("fix-login".into()), None, None, None).unwrap(),
        Target::Workspace(Some(name)) if name == "fix-login"
    ));
    for (repository, post) in [(Some("repo".into()), None), (None, Some(true))] {
        assert!(Target::new(Some("fix-login".into()), None, repository, post).is_err());
    }
}
