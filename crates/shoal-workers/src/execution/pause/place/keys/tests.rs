use super::*;

#[test]
fn enter_wakes_ctrl_c_stops_and_other_keys_keep_waiting() {
    assert_eq!(key_in(b"\r"), Some(Key::Wake));
    // A terminal that maps a carriage return to a newline.
    assert_eq!(key_in(b"\n"), Some(Key::Wake));
    assert_eq!(key_in(b"\x03"), Some(Key::Stop));
    assert_eq!(key_in(b"\x1c"), Some(Key::Stop));
    assert_eq!(key_in(b"q"), None);
    // An arrow key's escape sequence and a suspend key decide nothing.
    assert_eq!(key_in(b"\x1b[A\x1a"), None);
    // The first deciding key in what one read returns decides.
    assert_eq!(key_in(b"ab\x03\r"), Some(Key::Stop));
    assert_eq!(key_in(b"ab\r\x03"), Some(Key::Wake));
}

#[test]
fn the_key_mode_reads_each_key_unechoed_and_without_signals() {
    // SAFETY: an all-zero termios is a valid value to transform.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    settings.c_lflag = libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN | libc::ECHOE;
    settings.c_iflag = libc::IXON | libc::ICRNL;
    let keys = key_mode(settings);
    assert_eq!(keys.c_lflag, libc::ECHOE);
    // Enter still reaches the screen as a newline.
    assert_eq!(keys.c_iflag, libc::ICRNL);
    assert_eq!(keys.c_cc[libc::VMIN], 1);
    assert_eq!(keys.c_cc[libc::VTIME], 0);
}
