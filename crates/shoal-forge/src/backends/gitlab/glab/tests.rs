use super::*;

#[test]
fn hosts_are_the_keys_under_the_hosts_mapping() {
    let config = "git_protocol: ssh\n\
            hosts:\n    gitlab.com:\n        token: secret\n        api_host: gitlab.com\n\
            \n    # a comment\n    'git.corp.example:8443':\n        token: other\n\
            editor: vim\nother:\n    not.a.host:\n";
    assert_eq!(hosts(config), ["gitlab.com", "git.corp.example:8443"]);
    assert!(hosts("git_protocol: ssh\n").is_empty());
}

#[test]
fn hosts_match_with_or_without_a_url_or_port() {
    assert!(matches_host(
        "https://Git.Corp.Example/",
        "git.corp.example"
    ));
    assert!(matches_host("git.corp.example:8443", "git.corp.example"));
    assert!(matches_host(
        "git.corp.example:8443",
        "git.corp.example:8443"
    ));
    assert!(!matches_host("git.corp.example", "git.corp.example:8443"));
    assert!(!matches_host("corp.example", "git.corp.example"));
}
