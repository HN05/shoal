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
    support::{fixture::Base, wait::wait_for},
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
