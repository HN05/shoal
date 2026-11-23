use super::*;

#[test]
fn the_lid_reads_from_the_root_domain_and_is_none_without_one() {
    let laptop = "+-o IOPMrootDomain  <class IOPMrootDomain>\n    {\n      \"AppleClamshellCausesSleep\" = Yes\n      \"AppleClamshellState\" = No\n    }\n";
    let lid = |closed, docked| Some(Lid { closed, docked });
    assert_eq!(super::lid(laptop), lid(false, false));
    let closed = laptop.replace("State\" = No", "State\" = Yes");
    assert_eq!(super::lid(&closed), lid(true, false));
    let docked = closed.replace("Sleep\" = Yes", "Sleep\" = No");
    assert_eq!(super::lid(&docked), lid(true, true));
    assert_eq!(super::lid(""), None);
}

#[test]
fn the_idle_time_and_sleep_timer_read_as_durations() {
    let hid = "+-o IOHIDSystem  <class IOHIDSystem>\n    {\n      \"HIDIdleTime\" = 125000000000\n    }\n";
    assert_eq!(idle_time(hid), Some(Duration::from_secs(125)));
    let pmset = "System-wide power settings:\nCurrently in use:\n Sleep On Power Button 1\n disksleep            10\n sleep                15 (sleep prevented by sharingd)\n displaysleep         10\n";
    assert_eq!(sleep_timer(pmset), Some(Duration::from_secs(15 * 60)));
    assert_eq!(sleep_timer(&pmset.replace("15 (", "0 (")), None);
}
