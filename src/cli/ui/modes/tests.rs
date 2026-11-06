use super::*;

fn built_in() -> BTreeMap<String, Mode> {
    crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap()
        .modes
        .clone()
}

/// Each entry's mode and the kind of space its label names.
fn listed(kind: Option<SpaceKind>, forced: Option<SpaceKind>) -> Vec<(String, String)> {
    mode_entries(&built_in(), kind, forced, Palette::stdout(true))
        .into_iter()
        .map(|(id, label)| {
            let starts_in = label.split_whitespace().last().unwrap().to_owned();
            (id, starts_in)
        })
        .collect()
}

fn pair(mode: &str, kind: &str) -> (String, String) {
    (mode.to_owned(), kind.to_owned())
}

#[test]
fn the_mode_picker_lists_the_usual_mode_first_with_its_kind_of_space() {
    assert_eq!(
        listed(None, None),
        [
            pair("worker", "workspace"),
            pair("chat", "chat"),
            pair("plan", "chat")
        ]
    );
    // A forced kind is the kind every mode starts in.
    assert_eq!(
        listed(Some(SpaceKind::Chat), Some(SpaceKind::Chat)),
        [
            pair("chat", "chat"),
            pair("plan", "chat"),
            pair("worker", "chat")
        ]
    );
    assert_eq!(
        listed(Some(SpaceKind::Workspace), Some(SpaceKind::Workspace)),
        [
            pair("worker", "workspace"),
            pair("chat", "workspace"),
            pair("plan", "workspace")
        ]
    );
    // Without one, a start in one kind of space lists only its modes.
    assert_eq!(
        listed(Some(SpaceKind::Chat), None),
        [pair("chat", "chat"), pair("plan", "chat")]
    );
}

#[test]
fn a_start_takes_the_only_mode_of_its_kind_without_asking() {
    let ctx = Context::new(
        crate::paths::Paths::for_test(std::path::Path::new("state")),
        true,
    );
    let pick = ModeChoice::Pick { forced: None };
    let chosen = pick.chosen(&ctx, &built_in(), Some(SpaceKind::Workspace));
    assert_eq!(chosen.unwrap(), "worker");
    let named = ModeChoice::Named("missing".into());
    assert!(named.chosen(&ctx, &built_in(), None).is_err());
}
