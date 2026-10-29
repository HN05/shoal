use super::*;
use crate::sessions::zmx::parse_list;

#[test]
fn names_fit_the_socket_path_and_skip_taken_ones() {
    let taken = HashSet::from(["fix".to_owned(), "fix-2".to_owned()]);
    assert_eq!(free_name("fix", &taken), "fix-3");
    assert_eq!(free_name("other", &taken), "other");
    assert_eq!(free_name(&"x".repeat(40), &taken), "x".repeat(MAX_NAME_LEN));
}

#[test]
fn relabelling_replaces_the_other_owner_kind_and_a_demo() {
    assert_eq!(
        replacement_labels(Owner::Workspace("w1")),
        "shoal.workspace=w1 shoal.chat= shoal.demo="
    );
    assert_eq!(
        replacement_labels(Owner::Chat("c1")),
        "shoal.chat=c1 shoal.workspace= shoal.demo="
    );
}

#[test]
fn a_queued_start_from_a_session_goes_to_the_default_host_first() {
    assert!(Zmx.takes_queued(None));
    assert!(!Zmx.takes_queued(Some(&Origin::created(HostName::Zmx, "s".into()))));
    assert!(Zmx.takes_queued(Some(&Origin::created(HostName::Rex, "b".into()))));
}

#[test]
fn a_watch_follows_only_the_session_it_began_with() {
    let sessions = parse_list(
        "  name=fix\tpid=7\tcreated=100\tshoal.workspace=w1\n\
         \x20 name=other\tpid=8\tcreated=100\tshoal.workspace=w2\n\
         \x20 name=bare\tpid=9\tcreated=100\n",
    );
    let watched = |id: &str, instance: Option<&str>| Watched {
        id: id.into(),
        workspace: "w1".into(),
        instance: instance.map(str::to_owned),
    };
    assert_eq!(instance(&sessions[0].1), "7:100");
    assert!(hosts(&sessions, &watched("fix", Some("7:100"))));
    assert!(hosts(&sessions, &watched("fix", None)));
    // A later session of the same workspace under the same name.
    assert!(!hosts(&sessions, &watched("fix", Some("6:90"))));
    // Another workspace's session, an unlabelled one, or none at all.
    assert!(!hosts(&sessions, &watched("other", None)));
    assert!(!hosts(&sessions, &watched("bare", None)));
    assert!(!hosts(&sessions, &watched("gone", None)));
}

#[test]
fn a_failed_demo_worker_ends_its_shell() -> Result<()> {
    let command = session_command(&["false"], &NewSession::default());
    let root = tempfile::tempdir()?;
    let marker = root.path().join("shell-survived");
    let status = std::process::Command::new("sh")
        .args([
            "-c",
            &format!("{}\ntouch \"$1\"", command.trim_end_matches('\r')),
            "sh",
        ])
        .arg(&marker)
        .status()?;
    assert!(!status.success());
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn a_recorded_session_is_only_the_instance_it_recorded() {
    let sessions = parse_list("  name=shared\tpid=2\tcreated=20\n");
    let (name, fields) = &sessions[0];
    let mut place = Origin::created(HostName::Zmx, "shared".into());
    assert!(is_instance(name, fields, &place));
    place.instance = Some("2:20".into());
    assert!(is_instance(name, fields, &place));
    place.instance = Some("1:10".into());
    assert!(!is_instance(name, fields, &place));
}
