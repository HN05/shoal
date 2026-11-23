use super::*;

#[test]
fn a_lid_is_a_switch_device_setting_the_lid_bit() {
    let desktop = "N: Name=\"Power Button\"\nB: EV=3\n\nN: Name=\"HDA HDMI\"\nB: SW=140\n";
    assert!(!has_lid(desktop));
    let laptop = format!("{desktop}\nN: Name=\"Lid Switch\"\nB: EV=21\nB: SW=1\n");
    assert!(has_lid(&laptop));
    assert!(!has_lid(""));
}

#[test]
fn only_a_session_the_user_works_in_can_idle() {
    assert!(can_idle("user"));
    assert!(!can_idle("manager"));
    assert!(!can_idle("background"));
}
