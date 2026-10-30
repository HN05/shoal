use super::*;

#[test]
fn install_seeds_the_chat_template_and_preserves_edits() {
    let directory = tempfile::tempdir().unwrap();
    install_at(directory.path()).unwrap();
    let path = directory.path().join(CHAT_FILE);
    assert_eq!(fs::read_to_string(&path).unwrap(), CHAT_DEFAULT);
    assert!(!directory.path().join(WORKER_FILE).exists());
    fs::write(&path, "custom").unwrap();
    install_at(directory.path()).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "custom");
}

#[test]
fn the_guidance_tells_each_launch_how_it_receives_updates() {
    let workspace = workspace();
    let pushed = instructions(None, &workspace, Delivery::Pushed);
    assert!(
        pushed.contains("end your\nturn to wait for the next."),
        "{pushed}"
    );
    assert!(!pushed.contains("shoal watch pr"), "{pushed}");
    let watched = instructions(None, &workspace, Delivery::Watched);
    assert!(watched.contains("open, run `shoal watch pr`."), "{watched}");
    assert!(!watched.contains("{updates}"));
}

#[test]
fn additions_follow_the_current_guidance() {
    let workspace = workspace();
    let guidance = instructions(None, &workspace, Delivery::Watched);
    assert!(guidance.starts_with("You are working in Shoal workspace topic"));
    assert_eq!(
        instructions(Some(""), &workspace, Delivery::Watched),
        guidance
    );
    assert_eq!(
        instructions(Some("Babysit {branch}."), &workspace, Delivery::Watched),
        format!("{guidance}\n\nBabysit topic.")
    );
}

#[test]
fn a_copy_of_the_guidance_opening_repeats_it() {
    let earlier_default = "You are working in Shoal workspace {workspace} on branch {branch} at {path}.\n\
             Follow the repository's instructions.";
    assert!(repeats_guidance(earlier_default));
    assert!(!repeats_guidance("Babysit {branch}."));
}

fn workspace() -> crate::model::Workspace {
    serde_json::from_value(serde_json::json!({
        "id": "id", "repository_id": "repo", "name": "topic", "branch": "topic",
        "path": "/work/topic", "state": "ready", "created_at": 0, "updated_at": 0,
    }))
    .unwrap()
}
