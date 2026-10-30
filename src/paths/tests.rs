use std::path::PathBuf;

use super::*;

#[test]
fn the_client_crate_finds_the_socket_shoal_listens_on() {
    assert_eq!(shoal_client::STATE_DIR_VARIABLE, crate::env::STATE_DIR);
    let home = crate::fsutil::home_dir().unwrap();
    for state in [
        None,
        Some(PathBuf::from("/var/shoal")),
        Some(PathBuf::from("relative/state")),
    ] {
        let shoal = Paths::new(state.clone()).unwrap().socket;
        let client = shoal_client::socket_in(state.as_deref(), &home).unwrap();
        assert_eq!(client, shoal, "state directory {state:?}");
    }
}
