use super::*;
use crate::config::repo::{RepoConfig, parse};

fn config(text: &str) -> RepoConfig {
    parse(text).unwrap()
}

fn named(config: &RepoConfig) -> &[Preset] {
    config.presets.as_deref().unwrap_or_default()
}

fn labels(build: &Build) -> Vec<String> {
    build.steps().into_iter().map(|step| step.label).collect()
}

#[test]
fn presets_build_and_keep_what_their_tools_use() {
    let worktree = Path::new("/seed/worktree");
    let presets = config("presets = ['cargo', 'bun']\n");
    let build = Build::of(&presets.seed, named(&presets), worktree).unwrap();
    assert_eq!(
        labels(&build),
        ["cargo build --all-targets", "bun install --frozen-lockfile"]
    );
    assert_eq!(
        outputs(&presets.seed, named(&presets)),
        ["target", "node_modules"]
    );

    // A command and paths of the repository's own replace the presets'.
    let own = config("presets = ['cargo']\n[seed]\ncmd = 'seed.sh'\npaths = ['out']\n");
    let build = Build::of(&own.seed, named(&own), worktree).unwrap();
    let steps = build.steps();
    assert_eq!(steps[0].label, "seed.cmd");
    assert_eq!(steps[0].argv, [worktree.join("seed.sh").into_os_string()]);
    assert_eq!(outputs(&own.seed, named(&own)), ["out"]);
    // Go keeps its cache outside the worktree, so its seed has no outputs.
    let go = config("presets = ['go']\n");
    let build = Build::of(&go.seed, named(&go), worktree).unwrap();
    assert_eq!(labels(&build), ["go build ./..."]);
    assert!(outputs(&go.seed, named(&go)).is_empty());
    let none = config("presets = ['xcode']\n");
    assert!(Build::of(&none.seed, named(&none), worktree).is_none());
}

#[test]
fn presets_are_named_once_and_keep_distinct_outputs() {
    for (text, expected) in [
        ("presets = ['npm', 'npm']\n", "twice"),
        ("presets = ['npm', 'pnpm']\n", "both keep node_modules"),
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
