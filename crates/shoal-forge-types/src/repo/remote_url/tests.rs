use super::*;
use crate::ForgeRepo;

#[test]
fn registration_and_forge_policies_share_parsed_syntax() {
    for (source, key, forge_host, web_scheme) in [
        (
            "https://user:secret@EXAMPLE.com/team/Repo.git///",
            "remote:example.com/team/Repo",
            Some("example.com"),
            "https",
        ),
        (
            "http://example.com:3000/team/Repo",
            "remote:example.com:3000/team/Repo",
            Some("example.com:3000"),
            "http",
        ),
        (
            "ssh://git@EXAMPLE.com:2222/team/Repo.git/",
            "remote:example.com:2222/team/Repo",
            Some("example.com"),
            "https",
        ),
        (
            "git@EXAMPLE.com:team/Repo.git",
            "remote:example.com/team/Repo",
            Some("example.com"),
            "https",
        ),
        (
            "example.com:team/Repo",
            "remote:example.com/team/Repo",
            Some("example.com"),
            "https",
        ),
        (
            "git://example.com/team/Repo.git",
            "remote:example.com/team/Repo",
            None,
            "",
        ),
        ("file:///team/Repo.git", "file:///team/Repo.git", None, ""),
        (
            "HTTPS://example.com/team/Repo",
            "HTTPS://example.com/team/Repo",
            None,
            "",
        ),
        ("https://example.com", "https://example.com", None, ""),
        ("https:///team/Repo.git", "remote:/team/Repo", None, ""),
        (":team/Repo.git", "remote:/team/Repo", None, ""),
        (
            "https://-example.com/team/Repo",
            "remote:-example.com/team/Repo",
            None,
            "",
        ),
        (
            "https://example.com/group/team/Repo",
            "remote:example.com/group/team/Repo",
            None,
            "",
        ),
        (
            "https://example.com/team/../",
            "remote:example.com/team/..",
            None,
            "",
        ),
        (
            "https://example.com/team//Repo",
            "remote:example.com/team//Repo",
            None,
            "",
        ),
        (
            "https://example.com/team/Repo?query",
            "remote:example.com/team/Repo?query",
            None,
            "",
        ),
        (
            "https://example.com/team/Repo#fragment",
            "remote:example.com/team/Repo#fragment",
            None,
            "",
        ),
        (
            "https://example.com/team/repo%20name",
            "remote:example.com/team/repo%20name",
            None,
            "",
        ),
        (
            "https://example.com/team/répo",
            "remote:example.com/team/répo",
            None,
            "",
        ),
    ] {
        let remote = RemoteUrl::parse(source).unwrap();
        assert_eq!(remote.registration_key(), key, "{source}");
        let forge = ForgeRepo::parse(source);
        if let Some(host) = forge_host {
            let forge = forge.unwrap();
            assert_eq!(forge.host, host, "{source}");
            assert_eq!(forge.path, "team/Repo", "{source}");
            assert_eq!(forge.web_scheme, web_scheme, "{source}");
        } else {
            assert!(forge.is_err(), "{source}");
        }
    }
    assert!(RemoteUrl::parse("/local/checkout").is_none());
}
