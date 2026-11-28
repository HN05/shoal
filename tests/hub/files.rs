//! A space's attached files from another joined machine: their records,
//! which name the machine that keeps each copy, and their bytes, which
//! that machine reads.
use std::fs;

use shoal_client::files::read;
use shoal_protocol::space_names::SpaceSelector;

use super::{
    directory::{join, listed, machine, workspace},
    following::app,
    numbers::reroute,
    support::{
        fixture::Base,
        install_test_script, printed,
        prompt::{self, AnswerOptions},
        wait::wait_for,
    },
};

/// A laptop joined to `hub` whose new workspace has `notes.md` attached,
/// with the workspace's ID once the hub lists it.
fn laptop_with_notes(hub: &Base) -> (Base, String) {
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, hub, "laptop");
    let space = workspace(&laptop, "notes");
    let notes = laptop.root.path().join("notes.md");
    fs::write(&notes, "plan").unwrap();
    laptop.ok(&["files", "add", notes.to_str().unwrap(), "--space", &space]);
    listed(hub, &space, &laptop_id);
    (laptop, space)
}

/// The record ID of the file `machine` lists first for `space`.
fn first_file(machine: &Base, space: &str) -> i64 {
    machine.ok(&["files", space])[0]["id"].as_i64().unwrap()
}

#[test]
fn another_machine_lists_and_reads_a_spaces_files_by_the_machine_that_keeps_them() {
    let hub = machine(true, "app");
    let (_laptop, space) = laptop_with_notes(&hub);
    let listed = hub.ok(&["files", &space]);
    let file = &listed[0];
    assert_eq!(file["machine"], "laptop", "{listed}");
    assert!(file.get("path").is_none(), "{listed}");
    let id = file["id"].as_i64().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let selected = SpaceSelector::id(&space.as_str().into());
    let bytes = runtime.block_on(async {
        let read = read(&app(&hub), selected, id).await.unwrap();
        read.to_end().await.unwrap()
    });
    assert_eq!(bytes, b"plan");
}

/// A laptop joined to `hub` that removed a workspace with `notes.md`
/// attached, with the workspace's ID and the record ID of its file, once
/// the hub no longer lists the workspace.
fn laptop_that_removed_notes(hub: &Base) -> (Base, String, i64) {
    let (laptop, space) = laptop_with_notes(hub);
    laptop.ok(&["rm", &space, "--wait", "--yes"]);
    wait_for("the hub to drop the removed space", || {
        let spaces = hub.ok(&["list"]);
        let mut spaces = spaces.as_array().unwrap().iter();
        (!spaces.any(|listed| listed["id"] == space)).then_some(())
    });
    let listed = hub.ok(&["files", &space]);
    assert_eq!(listed[0]["machine"], "laptop", "{listed}");
    let id = listed[0]["id"].as_i64().unwrap();
    (laptop, space, id)
}

#[test]
fn another_machine_lists_and_reads_the_files_a_removed_space_left() {
    let hub = machine(true, "app");
    let (_laptop, space, id) = laptop_that_removed_notes(&hub);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let selected = SpaceSelector::id(&space.as_str().into());
    let bytes = runtime.block_on(async {
        let read = read(&app(&hub), selected, id).await.unwrap();
        read.to_end().await.unwrap()
    });
    assert_eq!(bytes, b"plan");
}

#[test]
fn a_removed_spaces_files_on_another_machine_stay_outside_a_scope_that_misses_it() {
    let hub = machine(true, "app");
    let (_laptop, space, id) = laptop_that_removed_notes(&hub);
    let own = workspace(&hub, "own");
    let issued = hub.request(serde_json::json!({"issue_scope": {"space": own}}));
    let scoped = app(&hub).scope(issued["data"].as_str().unwrap().into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(read(&scoped, SpaceSelector::id(&space.as_str().into()), id));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    // Refused by the laptop, which keeps the files, as outside the scope.
    assert!(error.contains("scope"), "{error}");
}

#[test]
fn a_space_of_another_machine_reads_no_file_outside_its_scope() {
    let hub = machine(true, "app");
    let (_laptop, space) = laptop_with_notes(&hub);
    let id = first_file(&hub, &space);
    let own = workspace(&hub, "own");
    let issued = hub.request(serde_json::json!({"issue_scope": {"space": own}}));
    let token = issued["data"].as_str().unwrap();
    let scoped = app(&hub).scope(token.into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(read(&scoped, SpaceSelector::id(&space.as_str().into()), id));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("scope"), "{error}");
}

#[test]
fn reading_a_file_on_an_offline_machine_fails_at_once_naming_it() {
    let hub = machine(true, "app");
    let (laptop, space) = laptop_with_notes(&hub);
    let id = first_file(&hub, &space);
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(read(
        &app(&hub),
        SpaceSelector::id(&space.as_str().into()),
        id,
    ));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("machine laptop is offline"), "{error}");
}

/// A picker that takes its first row, the first file, with the key
/// `SHOAL_TEST_PICK_KEY`, as fzf's `--expect` reports it.
const PICKER: &str = "#!/bin/sh\nrows=$(cat)\nprintf '%s\\n' \"$SHOAL_TEST_PICK_KEY\"\nprintf '%s\\n' \"$rows\" | head -n 1\n";

/// What `shoal files <space>` on `machine` prints on a terminal once its
/// picker takes the first file with `key`, which is empty for Enter.
fn picked(machine: &Base, space: &str, key: &str) -> String {
    install_test_script(machine.root.path().join("bin/fzf"), PICKER);
    let mut files = machine.command();
    files.env("SHOAL_TEST_PICK_KEY", key).args(["files", space]);
    let options = AnswerOptions {
        terminal_stdout: true,
        ..AnswerOptions::default()
    };
    let (output, transcript) = prompt::answer(&mut files, "", options);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    transcript
}

#[test]
fn the_files_picker_prints_and_opens_a_file_another_machine_keeps() {
    let hub = machine(true, "app");
    let (_laptop, space) = laptop_with_notes(&hub);
    assert!(picked(&hub, &space, "").contains("plan"));
    // The desktop's opener, which records what it opens.
    let opened = hub.root.path().join("opened");
    for opener in ["xdg-open", "open"] {
        let script = format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", opened.display());
        install_test_script(hub.root.path().join("bin").join(opener), script);
    }
    picked(&hub, &space, "ctrl-o");
    // The shell creates the file before `printf` writes the path into it.
    let copy = wait_for("the opener", || {
        fs::read_to_string(&opened)
            .ok()
            .filter(|path| !path.is_empty())
    });
    assert!(copy.contains("/.fetched/"), "{copy}");
    assert_eq!(fs::read_to_string(&copy).unwrap(), "plan");
}
