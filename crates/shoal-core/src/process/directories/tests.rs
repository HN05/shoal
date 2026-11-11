use super::command_name;

#[test]
fn command_name_decodes_bytes_that_are_not_utf8() {
    // "näme" cut inside "ä", as the kernel truncates a long name.
    assert_eq!(command_name(b"n\xc3\n"), "n\u{fffd}");
    assert_eq!(command_name(b"zellij\n"), "zellij");
}
