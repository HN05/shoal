use super::{check_branch_name, switch_selectors};
use crate::{git::PROGRAM, test_support::isolate};

#[test]
fn a_bare_name_completes_only_where_one_remote_alone_has_it() {
    let refs = [
        "refs/heads/main",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/shared",
        "refs/remotes/upstream/shared",
        "refs/remotes/origin/solo",
    ]
    .map(String::from);
    assert_eq!(
        switch_selectors(&refs),
        [
            "main",
            "origin/main",
            "origin/shared",
            "upstream/shared",
            "origin/solo",
            "solo",
        ]
    );
}

#[test]
fn branch_names_are_those_git_accepts() {
    let outside = tempfile::tempdir().unwrap();
    let names = [
        "fix",
        "a/b",
        "@",
        "a/@",
        "@/a",
        "refs/heads/x",
        "x.lock.y",
        "ü",
        "a@b",
        "a{b",
        "HEAD/x",
        "foo/HEAD",
        "a-",
        "a@",
        "@a",
        "@{-1}",
        "-x",
        "-",
        "",
        "a.lock",
        "a/.b",
        ".a",
        "a..b",
        "...",
        "a/./b",
        "a/",
        "/a",
        "a//b",
        "a.",
        "a@{b",
        "a b",
        "a~b",
        "a^b",
        "a:b",
        "a?b",
        "a*b",
        "a[b",
        "a\\b",
        "a\tb",
        "a\x7fb",
        "x\n",
        "a/b.lock/c",
        "a.lock/b",
        ".lock",
        "x/.lock",
        "a.lock.",
    ];
    for name in names {
        let output = isolate(&mut std::process::Command::new(PROGRAM), outside.path())
            .args(["check-ref-format", "--branch", name])
            .output()
            .unwrap();
        assert_eq!(
            check_branch_name(name).is_ok(),
            output.status.success(),
            "{name:?}"
        );
    }
}

#[test]
fn a_new_branch_may_take_the_name_head() {
    check_branch_name("HEAD").unwrap();
}
