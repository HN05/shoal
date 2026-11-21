//! The shared config across the hub: an edit to `shared.toml` or a file
//! beside it on one machine reaches every other, an edit made offline goes
//! once the link opens, and a local edit a copy replaces stays as a backup.
use std::{fs, path::PathBuf};

use super::{
    directory::{join, machine},
    support::{fixture::Base, wait::wait_for},
};

/// The shared file `name`, below the global config's directory of
/// `machine`.
fn shared(machine: &Base, name: &str) -> PathBuf {
    machine.root.path().join(".config/shoal").join(name)
}

fn write(machine: &Base, name: &str, text: &str) {
    let path = shared(machine, name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Wait until the shared file `name` of `machine` reads `text`.
fn reads(machine: &Base, name: &str, text: &str) {
    wait_for(&format!("{name} to read {text:?}"), || {
        let read = fs::read_to_string(shared(machine, name)).ok();
        (read.as_deref() == Some(text)).then_some(())
    });
}

const IDLE_30: &str = "[cleanup.auto]\nidle_minutes = 30\n";

#[test]
fn an_edit_on_one_machine_reaches_every_other_and_a_replaced_edit_stays_as_a_backup() {
    let hub = machine(true, "app");
    let desk = machine(false, "app");
    let laptop = machine(false, "app");
    join(&desk, &hub, "desk");
    write(&desk, "shared.toml", IDLE_30);
    write(&desk, "shared/modes/review.md", "Review it.\n");
    desk.ok(&["config", "reload"]);
    reads(&hub, "shared/modes/review.md", "Review it.\n");
    reads(&hub, "shared.toml", IDLE_30);
    // The laptop's own file, which it never sent, gives way to the hub's.
    let own = "[cleanup.auto]\nidle_minutes = 45\n";
    write(&laptop, "shared.toml", own);
    join(&laptop, &hub, "laptop");
    reads(&laptop, "shared.toml", IDLE_30);
    reads(&laptop, "shared/modes/review.md", "Review it.\n");
    reads(&laptop, "shared.toml.backup", own);
    let alerts = laptop.ok(&["alert", "list", "--unread"]);
    let kept: Vec<_> = (alerts.as_array().unwrap().iter())
        .filter(|alert| alert["kind"] == "shared_file_kept")
        .collect();
    let [kept] = kept.as_slice() else {
        panic!("{alerts}");
    };
    let backup = shared(&laptop, "shared.toml.backup");
    let message = kept["message"].as_str().unwrap();
    assert!(message.contains(backup.to_str().unwrap()), "{message}");
    // An edit on the hub's own machine reaches both.
    hub.ok(&[
        "config",
        "set",
        "--shared",
        "cleanup.auto.idle_minutes",
        "50",
    ]);
    let edited = fs::read_to_string(shared(&hub, "shared.toml")).unwrap();
    reads(&desk, "shared.toml", &edited);
    reads(&laptop, "shared.toml", &edited);
}

#[test]
fn an_edit_made_offline_goes_once_the_link_opens() {
    let hub = machine(true, "app");
    let mut laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    hub.ok(&[
        "config",
        "set",
        "--shared",
        "cleanup.auto.idle_minutes",
        "30",
    ]);
    let agreed = fs::read_to_string(shared(&hub, "shared.toml")).unwrap();
    reads(&laptop, "shared.toml", &agreed);
    laptop.ok(&["daemon", "stop"]);
    let offline = "[cleanup.auto]\nidle_minutes = 45\n";
    write(&laptop, "shared.toml", offline);
    write(&hub, "shared/rules/tests.md", "Test it.\n");
    hub.ok(&["config", "reload"]);
    laptop.restart();
    reads(&hub, "shared.toml", offline);
    reads(&laptop, "shared/rules/tests.md", "Test it.\n");
    assert!(!shared(&laptop, "shared.toml.backup").exists());
}
