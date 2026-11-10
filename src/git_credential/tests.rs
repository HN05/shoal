use super::{helper_covers, settings};
use crate::paths::Paths;

#[test]
fn each_host_gets_a_reset_then_shoal_s_helper() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());

    let settings = settings(&paths, ["git.example.com:3000"], &[]).unwrap();

    let key = "credential.https://git.example.com:3000.helper";
    let [(reset_key, reset), (helper_key, helper)] = settings.as_slice() else {
        panic!("expected a reset and a helper, got {settings:?}");
    };
    assert_eq!((reset_key.as_str(), reset.as_str()), (key, ""));
    assert_eq!(helper_key, key);
    assert!(helper.starts_with('!'), "{helper}");
    assert!(helper.ends_with(" internal git-credential"), "{helper}");
}

#[test]
fn a_profile_helper_that_covers_a_host_keeps_shoal_s_away_from_it() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let profile = [(
        "credential.https://git.example.com.helper".to_owned(),
        "store".to_owned(),
    )];

    let settings = settings(&paths, ["git.example.com", "code.example.com"], &profile).unwrap();

    let hosts: Vec<_> = settings.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        hosts,
        [
            "credential.https://code.example.com.helper",
            "credential.https://code.example.com.helper",
        ]
    );
}

#[test]
fn helpers_cover_the_hosts_git_asks_them_for() {
    let host = "git.example.com";
    let covering = [
        "credential.helper",
        "Credential.Helper",
        "credential.https://git.example.com.helper",
        "credential.https://Git.Example.com/.helper",
        "credential.https://git.example.com:443.helper",
        "credential.git.example.com.helper",
        "credential.https://*.example.com.helper",
    ];
    let other = [
        "credential.http://git.example.com.helper",
        "credential.https://code.example.com.helper",
        "credential.https://git.example.com:3000.helper",
        "credential.https://*.git.example.com.helper",
        "credential.https://agent@git.example.com.helper",
        "credential.https://git.example.com/team/repo.git.helper",
        "credential.https://git.example.com.username",
        "user.helper",
    ];

    let covered: Vec<_> = covering
        .into_iter()
        .filter(|key| !helper_covers(key, host))
        .collect();
    let uncovered: Vec<_> = other
        .into_iter()
        .filter(|key| helper_covers(key, host))
        .collect();
    assert!(covered.is_empty(), "these should cover {host}: {covered:?}");
    assert!(
        uncovered.is_empty(),
        "these should not cover {host}: {uncovered:?}"
    );
}
