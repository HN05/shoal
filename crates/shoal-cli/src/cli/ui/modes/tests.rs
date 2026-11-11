use super::*;

fn built_in() -> BTreeMap<String, Mode> {
    crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap()
        .modes
        .clone()
}

/// Each entry's mode and the space its label names.
fn listed(fit: ModeFit) -> Vec<(String, String)> {
    mode_entries(&built_in(), fit, Palette::stdout(true))
        .into_iter()
        .map(|(id, label)| {
            let space = label.trim_start_matches(id.as_str()).trim().to_owned();
            (id, space)
        })
        .collect()
}

fn pair(mode: &str, space: &str) -> (String, String) {
    (mode.to_owned(), space.to_owned())
}

#[test]
fn the_mode_picker_lists_the_usual_mode_first_with_the_space_it_starts() {
    assert_eq!(
        listed(ModeFit::default()),
        [
            pair("worker", "workspace"),
            pair("chat", "chat"),
            pair("orchestrate", "chat without a repository"),
            pair("plan", "chat")
        ]
    );
    // A start of one kind of space lists only its modes.
    let chat = ModeFit::in_repository(SpaceKind::Chat);
    assert_eq!(listed(chat), [pair("chat", "chat"), pair("plan", "chat")]);
}

#[test]
fn a_start_takes_the_only_mode_of_its_kind_without_asking() {
    let ctx = Context::new(
        crate::paths::Paths::for_test(std::path::Path::new("state")),
        true,
    );
    let pick = ModeChoice::Pick;
    let fit = ModeFit::in_repository(SpaceKind::Workspace);
    assert_eq!(pick.chosen(&ctx, &built_in(), fit).unwrap(), "worker");
    let named = ModeChoice::Named("missing".into());
    assert!(named.chosen(&ctx, &built_in(), ModeFit::default()).is_err());
}
