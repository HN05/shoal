use shoal_forge_types::items::reference::ItemRef;
use shoal_protocol::model::MergePath;

use super::*;

fn pull(number: u64) -> ItemRef {
    format!("https://forge.example/team/app/pulls/{number}")
        .parse()
        .unwrap()
}

#[test]
fn one_open_pr_merges_on_the_forge() {
    let path = choose(true, "fix", vec![pull(12)]).unwrap();
    assert_eq!(path, MergePath::Pr { pr: pull(12) });
    let path = choose(false, "talk", vec![pull(12)]).unwrap();
    assert_eq!(path, MergePath::Pr { pr: pull(12) });
}

#[test]
fn a_workspace_without_an_open_pr_merges_locally() {
    assert_eq!(choose(true, "fix", Vec::new()).unwrap(), MergePath::Local);
}

#[test]
fn a_chat_without_an_open_pr_is_refused() {
    let refused = choose(false, "talk", Vec::new()).unwrap_err().to_string();
    assert!(refused.contains("no open PR"), "{refused}");
}

#[test]
fn several_open_prs_are_refused_naming_each() {
    let refused = choose(true, "fix", vec![pull(12), pull(14)])
        .unwrap_err()
        .to_string();
    assert!(refused.contains("/pulls/12"), "{refused}");
    assert!(refused.contains("/pulls/14"), "{refused}");
}
