use super::switch_selectors;

#[test]
fn a_bare_name_completes_only_where_one_remote_alone_has_it() {
    let refs = "refs/heads/main\n\
                refs/remotes/origin/HEAD\n\
                refs/remotes/origin/main\n\
                refs/remotes/origin/shared\n\
                refs/remotes/upstream/shared\n\
                refs/remotes/origin/solo\n";
    assert_eq!(
        switch_selectors(refs),
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
