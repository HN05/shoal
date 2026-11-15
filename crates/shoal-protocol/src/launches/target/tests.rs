use super::*;

/// What `input` names once the start knows whether it names a branch.
fn decided(input: &str, names_branch: bool) -> Result<StartTarget> {
    input.parse::<StartTarget>()?.decided(names_branch)
}

#[test]
fn a_number_or_issue_link_names_an_issue() {
    for input in ["42", "#42"] {
        let target = decided(input, false).unwrap();
        assert_eq!(target, StartTarget::Issue(ItemInput::Number(42)), "{input}");
    }
    let issue = "https://github.com/team/app/issues/7";
    let target = decided(issue, false).unwrap();
    assert!(matches!(target, StartTarget::Issue(ItemInput::Url(item)) if item.url() == issue));
}

#[test]
fn a_number_beside_a_branch_names_the_repository_as_given() {
    let overlong = "99999999999999999999";
    for input in ["007", "0", overlong] {
        let target = decided(input, true).unwrap();
        assert_eq!(target, StartTarget::Repository(input.into()), "{input}");
    }
    assert!(decided("0", false).is_err());
    assert!(decided(overlong, false).is_err());
}

#[test]
fn a_pr_or_branch_link_names_its_repository() {
    let pull = "https://github.com/team/app/pull/8".parse::<StartTarget>();
    assert!(matches!(pull, Ok(StartTarget::Pull(_))));
    let branch = "https://forge.example/team/app/src/branch/fix%2Fa"
        .parse::<StartTarget>()
        .unwrap();
    let StartTarget::Branch { branch, .. } = &branch else {
        panic!("not a branch: {branch:?}");
    };
    assert_eq!(branch, "fix/a");
    for target in [
        pull.unwrap(),
        "https://forge.example/team/app/src/branch/fix"
            .parse()
            .unwrap(),
    ] {
        let repository = target.linked_repository().unwrap();
        assert_eq!(repository.path, "team/app");
    }
}

#[test]
fn anything_else_names_a_repository() {
    for input in ["my-project", "https://github.com/team/app", "./app"] {
        let target = input.parse::<StartTarget>().unwrap();
        assert!(matches!(&target, StartTarget::Repository(named) if named == input));
        assert!(target.linked_repository().is_none());
    }
}

#[test]
fn a_run_or_an_invalid_item_link_is_refused() {
    for input in [
        "https://github.com/team/app/actions/runs/9",
        "https://github.com/team/app/issues/0",
    ] {
        assert!(input.parse::<StartTarget>().is_err(), "{input}");
    }
}
