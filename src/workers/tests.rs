use super::*;

#[test]
fn shoal_argv_names_even_the_default_state_directory() {
    let default = Paths {
        state: "/home/.local/state/shoal".into(),
        ..Paths::for_test("/home")
    };
    assert_eq!(
        shoal_argv(&default).unwrap()[1..],
        ["--state-dir", "/home/.local/state/shoal", "--text"]
    );
}
