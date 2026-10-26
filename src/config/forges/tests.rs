use super::*;

#[test]
fn hosts_pin_a_known_type_case_insensitively() {
    let forges: Forges = toml::from_str("[\"Git.Example.com\"]\ntype = \"gitlab\"\n").unwrap();
    forges.validate().unwrap();
    assert_eq!(forges.pinned("git.example.com"), Some(ForgeKind::GitLab));
    assert_eq!(forges.pinned("other.example.com"), None);
    assert!(toml::from_str::<Forges>("[\"git.example.com\"]\ntype = \"svn\"\n").is_err());
    assert!(toml::from_str::<Forges>("[\"git.example.com\"]\nkind = \"github\"\n").is_err());
    let url: Forges = toml::from_str("[\"https://git.example.com\"]\ntype = \"github\"\n").unwrap();
    assert!(url.validate().is_err());
    let twice: Forges = toml::from_str(
        "[\"Git.example.com\"]\ntype = \"github\"\n[\"git.example.com\"]\ntype = \"gitlab\"\n",
    )
    .unwrap();
    assert!(twice.validate().is_err());
}
