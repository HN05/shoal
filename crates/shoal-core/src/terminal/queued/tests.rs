use super::*;
use crate::terminal::detached_start_environment;

#[test]
fn a_start_from_no_terminal_records_no_origin_and_no_server() {
    let env = detached_start_environment(None);
    assert_eq!(recorded_origin(&env[env::ORIGIN]).unwrap(), None);
    assert_eq!(env["HERDR_SESSION"], "");
    assert_eq!(env["REX_SERVER"], "");
}

#[test]
fn a_start_from_no_terminal_records_the_origin_it_opens_in() {
    let origin = Origin {
        host: crate::sessions::HostName::Herdr,
        group: Some("work".into()),
        place: Some("pane".into()),
        data: Default::default(),
        owned: false,
        instance: None,
    };
    let env = detached_start_environment(Some(&origin));
    assert_eq!(recorded_origin(&env[env::ORIGIN]).unwrap(), Some(origin));
    assert_eq!(env["HERDR_SESSION"], "");
}
