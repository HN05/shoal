use super::*;

#[test]
fn a_replaced_option_keeps_the_other_arguments() {
    let arguments =
        |arguments: &[&str]| replace_option(arguments.iter().map(OsString::from), "--since", "9");
    assert_eq!(
        arguments(&["internal", "events", "--since", "3", "--follow"]),
        ["internal", "events", "--follow", "--since", "9"]
    );
    assert_eq!(
        arguments(&["--json", "internal", "events", "--since=3"]),
        ["--json", "internal", "events", "--since", "9"]
    );
    assert_eq!(
        arguments(&["internal", "events", "--follow"]),
        ["internal", "events", "--follow", "--since", "9"]
    );
}

#[test]
fn a_short_option_goes_with_its_value() {
    let arguments = |arguments: &[&str]| without_option(arguments.iter().map(OsString::from), "-s");
    assert_eq!(arguments(&["watch", "-s", "api", "pr"]), ["watch", "pr"]);
    assert_eq!(
        arguments(&["watch", "-sapi", "--space", "x"]),
        ["watch", "--space", "x"]
    );
}

#[test]
fn a_flag_is_added_once() {
    let arguments =
        |arguments: &[&str]| with_flag(arguments.iter().map(OsString::from).collect(), "--resume");
    assert_eq!(arguments(&["watch"]), ["watch", "--resume"]);
    assert_eq!(arguments(&["watch", "--resume"]), ["watch", "--resume"]);
}
