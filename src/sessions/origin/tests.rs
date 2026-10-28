use super::*;

#[test]
fn unknown_hosts_are_rejected() {
    assert!(serde_json::from_value::<HostName>(serde_json::json!("tmux")).is_err());
    assert!("tmux".parse::<HostName>().is_err());
    assert_eq!("rex".parse::<HostName>().unwrap(), HostName::Rex);
}
