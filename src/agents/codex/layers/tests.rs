use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn provider(config: &toml::Table) -> Option<&str> {
    config.get("model_provider")?.as_str()
}

#[test]
fn a_profile_file_layers_over_the_users_config() {
    let home = tempfile::tempdir().unwrap();
    write(
        &home.path().join("config.toml"),
        "model_provider = 'user'\nmodel = 'gpt'\n",
    );
    write(
        &home.path().join("work.config.toml"),
        "model_provider = 'work'\n",
    );
    let directory = home.path();
    let plain = effective(home.path(), None, directory).unwrap();
    assert_eq!(provider(&plain), Some("user"));
    let work = effective(home.path(), Some("work"), directory).unwrap();
    assert_eq!(provider(&work), Some("work"));
    // What the profile leaves alone stays the user's.
    assert_eq!(work["model"].as_str(), Some("gpt"));
    // The user's config may select the profile itself.
    write(
        &home.path().join("config.toml"),
        "profile = 'work'\nmodel_provider = 'user'\n",
    );
    let selected = effective(home.path(), None, directory).unwrap();
    assert_eq!(provider(&selected), Some("work"));
    // A profile without a file of its own changes nothing.
    let missing = effective(home.path(), Some("missing"), directory).unwrap();
    assert_eq!(provider(&missing), Some("user"));
}

#[test]
fn projects_apply_from_their_root_down_and_only_when_trusted() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex");
    let project = root.path().join("project");
    let nested = project.join("a/b");
    write(
        &project.join(".codex/config.toml"),
        "model_provider = 'root'\nmodel = 'root'\n",
    );
    write(
        &project.join("a/.codex/config.toml"),
        "model_provider = 'a'\n",
    );
    std::fs::create_dir_all(&nested).unwrap();
    // A worktree's `.git` is a file; it still marks the root.
    write(&project.join(".git"), "gitdir: elsewhere\n");
    let trust = format!(
        "[projects.{:?}]\ntrust_level = 'trusted'\n",
        project.to_str().unwrap()
    );
    write(
        &home.join("config.toml"),
        &format!("model_provider = 'user'\n{trust}"),
    );
    let config = effective(&home, None, &nested).unwrap();
    assert_eq!(provider(&config), Some("a"));
    assert_eq!(config["model"].as_str(), Some("root"));
    assert_eq!(
        provider(&effective(&home, None, &project).unwrap()),
        Some("root")
    );
    // Other root markers find the root elsewhere.
    write(
        &home.join("config.toml"),
        &format!("model_provider = 'user'\nproject_root_markers = ['.hg']\n{trust}"),
    );
    write(&project.join("a/.hg"), "");
    let config = effective(&home, None, &nested).unwrap();
    assert_eq!(provider(&config), Some("a"));
    // Untrusted, only the user's config applies.
    write(&home.join("config.toml"), "model_provider = 'user'\n");
    assert_eq!(
        provider(&effective(&home, None, &nested).unwrap()),
        Some("user")
    );
}
