use super::*;
use shoal_core::config::repo::{RepoConfig, parse};

fn config(text: &str) -> RepoConfig {
    parse(text).unwrap()
}

fn named(config: &RepoConfig) -> &[Preset] {
    config.setup.presets.as_deref().unwrap_or_default()
}

fn labels(build: &Build) -> Vec<String> {
    build
        .steps(&[])
        .into_iter()
        .map(|step| step.label)
        .collect()
}

#[test]
fn presets_build_and_keep_what_their_tools_use() {
    let worktree = Path::new("/seed/worktree");
    let presets = config("setup.presets = ['cargo', 'bun']\n");
    let build = Build::of(&presets.setup.seed, named(&presets), worktree).unwrap();
    assert_eq!(
        labels(&build),
        [
            "cargo build --all-targets",
            "cargo test --no-run",
            "cargo check --all-targets",
            "cargo clippy --all-targets",
            "bun install --frozen-lockfile"
        ]
    );
    assert_eq!(
        outputs(&presets.setup.seed, named(&presets)),
        ["target", "node_modules"]
    );

    // A command and paths of the repository's own replace the presets'.
    let own =
        config("setup.presets = ['cargo']\n[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n");
    let build = Build::of(&own.setup.seed, named(&own), worktree).unwrap();
    let steps = build.steps(&[]);
    assert_eq!(steps[0].label, "setup.seed.command");
    assert_eq!(steps[0].argv, [worktree.join("seed.sh").into_os_string()]);
    assert_eq!(outputs(&own.setup.seed, named(&own)), ["out"]);
    let none = config("setup.presets = ['xcode']\n");
    assert!(Build::of(&none.setup.seed, named(&none), worktree).is_none());
}

#[test]
fn presets_are_named_once_and_keep_distinct_outputs() {
    for (text, expected) in [
        ("setup.presets = ['npm', 'npm']\n", "twice"),
        (
            "setup.presets = ['npm', 'pnpm']\n",
            "both keep node_modules",
        ),
    ] {
        let error = parse(text).unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
}

#[test]
fn a_script_keeps_the_settings_of_earlier_snapshots() {
    let build = Build::Script("/seed/worktree/seed.sh".into());
    assert_eq!(
        serde_json::to_string(&build).unwrap(),
        serde_json::to_string(Path::new("/seed/worktree/seed.sh")).unwrap()
    );
}

#[test]
fn a_reset_runs_before_the_build_and_a_script_has_none() {
    let build = Build::Presets(vec![Preset::Bun, Preset::Cargo]);
    let resets = build.resets(None, "s", &Toolchains::new());
    assert_eq!(resets, [Preset::Cargo]);
    let mut expected = vec![Preset::Cargo.reset_command().unwrap().join(" ")];
    expected.extend(labels(&build));
    let run: Vec<_> = (build.steps(&resets).into_iter())
        .map(|step| step.label)
        .collect();
    assert_eq!(run, expected);
    let script = Build::Script("/seed/worktree/seed.sh".into());
    assert!(script.resets(None, "s", &Toolchains::new()).is_empty());
    assert!(script.toolchain_steps().is_empty());
}

/// The toolchains a Cargo build saw.
fn rust(version: &str) -> Toolchains {
    Toolchains::from([(Preset::Cargo, format!("rustc {version}\n"))])
}

/// A snapshot built under settings `s` with Rust 1.100, followed by
/// changes to `changed`.
fn previous(changed: &[&str]) -> Previous {
    Previous {
        settings: "s".into(),
        toolchains: rust("1.100"),
        changed: Some(changed.iter().map(|file| (*file).to_owned()).collect()),
    }
}

fn resets(previous: &Previous, settings: &str, toolchains: &Toolchains) -> Vec<Preset> {
    let build = Build::Presets(vec![Preset::Cargo, Preset::Bun]);
    build.resets(Some(previous), settings, toolchains)
}

#[test]
fn an_unchanged_build_keeps_its_outputs() {
    let changed = previous(&["src/lib.rs", "crates/a/src/Cargo.toml.rs", "bun.lock"]);
    assert!(resets(&changed, "s", &rust("1.100")).is_empty());
}

#[test]
fn a_build_resets_after_a_change_to_what_its_units_hash() {
    for file in ["Cargo.lock", "crates/a/Cargo.toml"] {
        assert_eq!(
            resets(&previous(&["src/lib.rs", file]), "s", &rust("1.100")),
            [Preset::Cargo],
            "{file}"
        );
    }
}

#[test]
fn a_build_resets_under_other_settings_or_toolchain() {
    let unchanged = previous(&[]);
    assert_eq!(resets(&unchanged, "t", &rust("1.100")), [Preset::Cargo]);
    assert_eq!(resets(&unchanged, "s", &rust("1.101")), [Preset::Cargo]);
    // A snapshot from before Shoal recorded toolchains, or of a commit the
    // repository no longer has.
    let unrecorded = Previous {
        toolchains: Toolchains::new(),
        ..previous(&[])
    };
    assert_eq!(resets(&unrecorded, "s", &rust("1.100")), [Preset::Cargo]);
    let unknown = Previous {
        changed: None,
        ..previous(&[])
    };
    assert_eq!(resets(&unknown, "s", &rust("1.100")), [Preset::Cargo]);
}
