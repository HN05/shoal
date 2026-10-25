use super::*;

fn demo(cmd: &[&str]) -> Demo {
    Demo {
        cmd: cmd.iter().map(|arg| (*arg).to_owned()).collect(),
        preset: None,
        open: None,
        ports: None,
    }
}

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn a_command_names_an_ad_hoc_demo_and_otherwise_the_configuration_decides() {
    let one = BTreeMap::from([("tui".to_owned(), demo(&["cargo", "run"]))]);
    let chosen = choose(&one, None, Vec::new()).unwrap();
    assert_eq!(chosen.name, "tui");
    assert_eq!(chosen.command, words(&["cargo", "run"]));

    let ad_hoc = choose(&one, None, words(&["./prototype"])).unwrap();
    assert_eq!(ad_hoc.name, DEFAULT_NAME);
    assert_eq!(ad_hoc.command, words(&["./prototype"]));
    // A command under a configured name keeps the rest of its definition.
    let mut web = demo(&["npm", "run", "dev"]);
    web.preset = Some(Preset::Web);
    let two = BTreeMap::from([
        ("tui".to_owned(), demo(&["cargo", "run"])),
        ("web".to_owned(), web),
    ]);
    let replaced = choose(&two, Some("web".into()), words(&["vite"])).unwrap();
    assert_eq!(replaced.command, words(&["vite"]));
    assert_eq!(replaced.demo.preset, Some(Preset::Web));

    let error = choose(&two, None, Vec::new()).unwrap_err().to_string();
    assert!(error.contains("tui, web"), "{error}");
    let error = choose(&two, Some("api".into()), Vec::new())
        .unwrap_err()
        .to_string();
    assert!(error.contains("configured demos: tui, web"), "{error}");
    assert!(choose(&BTreeMap::new(), None, Vec::new()).is_err());
    assert!(choose(&BTreeMap::new(), Some("ls".into()), words(&["x"])).is_err());
}

#[test]
fn start_options_replace_the_configured_parts() {
    let chosen = choose(
        &BTreeMap::from([("web".to_owned(), demo(&["serve"]))]),
        None,
        Vec::new(),
    )
    .unwrap()
    .with(Overrides {
        preset: Some(Preset::Web),
        open: Some("echo $PORT".into()),
        ports: vec!["api".into()],
    });
    assert_eq!(chosen.demo.preset, Some(Preset::Web));
    assert_eq!(
        chosen.demo.open.as_deref(),
        Some(&["sh".to_owned(), "-c".into(), "echo $PORT".into()][..])
    );
    assert_eq!(chosen.demo.ports.as_deref(), Some(&["api".to_owned()][..]));
}

#[test]
fn a_screen_keeps_its_last_lines_without_trailing_blanks() {
    assert_eq!(
        last_lines("one\ntwo\nthree\n\n\n", 2).as_deref(),
        Some("two\nthree")
    );
    assert_eq!(last_lines("one\n", 5).as_deref(), Some("one"));
    assert_eq!(last_lines("\n\n", 5), None);
}

#[test]
fn listings_show_running_demos_then_the_configured_ones_not_running() {
    let launch = Launch {
        name: "web".into(),
        preset: Some(Preset::Web),
        open: None,
        environment: BTreeMap::new(),
        host: "zmx".into(),
        place: "first-web".into(),
    };
    let listing = Listing {
        workspace: Some("first".into()),
        configured: BTreeMap::from([
            ("tui".to_owned(), demo(&["cargo", "run"])),
            ("web".to_owned(), demo(&["npm", "run", "dev"])),
        ]),
        running: vec![Running {
            workspace_id: "id".into(),
            workspace: "first".into(),
            execution_id: "execution".into(),
            started_at: 0,
            launch,
        }],
    };
    assert_eq!(
        listing_rows(&listing, Palette::stdout(true)),
        ["web  first  Running      web", "tui  first  Not running",]
    );
}
