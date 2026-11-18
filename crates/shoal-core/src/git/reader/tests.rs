use std::path::Path;

use super::read;
use crate::test_support::{git, reftable_repository, repository};

/// Which way [`read`] read the repository at `path`.
async fn reader(path: &Path) -> &'static str {
    read(path, |_| Ok("gix"), async |_| Ok("git"))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_repository_with_its_refs_in_files_reads_in_process() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["worktree", "add", "-b", "topic", "../topic"]);
    std::fs::create_dir(repo.join("nested")).unwrap();
    for path in [repo.clone(), repo.join("nested"), root.path().join("topic")] {
        assert_eq!(reader(&path).await, "gix", "{}", path.display());
    }
}

#[tokio::test]
async fn a_reftable_repository_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = reftable_repository(root.path(), "repo");
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn an_extension_gix_does_not_know_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "core.repositoryFormatVersion", "1"]);
    git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(reader(&repo).await, "gix");
    git(&repo, &["config", "extensions.someFutureFormat", "true"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_partial_clone_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "core.repositoryFormatVersion", "1"]);
    git(&repo, &["config", "extensions.partialClone", "origin"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_promisor_remote_without_the_extension_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(
        &repo,
        &["config", "remote.origin.url", "https://example.com/app"],
    );
    git(&repo, &["config", "remote.origin.promisor", "false"]);
    assert_eq!(reader(&repo).await, "gix");
    git(&repo, &["config", "remote.origin.promisor", "true"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_directory_outside_any_repository_reads_through_git() {
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(reader(outside.path()).await, "git");
    assert_eq!(reader(&outside.path().join("missing")).await, "git");
}

/// The variable that names the repository a helper reads.
const HELPER_REPOSITORY: &str = "SHOAL_TEST_HELPER_REPOSITORY";

/// The variables that name the installation and system config files
/// [`a_config_files_helper`] reads.
const HELPER_INSTALLATION: &str = "SHOAL_TEST_INSTALLATION_CONFIG";
const HELPER_SYSTEM: &str = "SHOAL_TEST_SYSTEM_CONFIG";

/// Run this binary's ignored test `helper` in a process of its own, which
/// reads `repo` in the environment `environment` gives it, apart from the
/// other tests' environment; it must pass.
fn run_helper(
    helper: &str,
    repo: &Path,
    environment: impl FnOnce(&mut std::process::Command) -> &mut std::process::Command,
) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    let name = format!("git::reader::tests::{helper}");
    command
        .args(["--exact", &name, "--ignored"])
        .env(HELPER_REPOSITORY, repo);
    let output = environment(&mut command).output().unwrap();
    let printed = crate::test_support::printed(&output);
    assert!(output.status.success(), "{printed}");
}

/// The repository a helper reads.
fn helper_repository() -> std::path::PathBuf {
    std::env::var_os(HELPER_REPOSITORY).unwrap().into()
}

#[test]
fn a_ref_namespace_leaves_reads_as_git_reads_them() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    run_helper("a_namespaced_read_helper", &repo, |command| {
        command.env("GIT_NAMESPACE", "elsewhere")
    });
}

/// Read refs Git sees whatever `GIT_NAMESPACE` names, in a process whose
/// environment sets it.
#[tokio::test]
#[ignore = "run by a_ref_namespace_leaves_reads_as_git_reads_them"]
async fn a_namespaced_read_helper() {
    let repo = helper_repository();
    assert!(
        crate::git::ref_exists(&repo, "refs/heads/main")
            .await
            .unwrap()
    );
    let (_, branch) = crate::git::head_and_branch(&repo).await.unwrap();
    assert_eq!(branch, Some("main".into()));
}

#[test]
fn the_config_files_of_the_git_shoal_runs_are_read() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let installation = root.path().join("installation.gitconfig");
    std::fs::write(&installation, "[shoal]\n\tinstallation = yes\n").unwrap();
    let system = root.path().join("system.gitconfig");
    std::fs::write(&system, "[shoal]\n\tsystem = yes\n").unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let bin = config_reporting_git(root.path(), &path);
    let search = std::env::join_paths([bin].into_iter().chain(std::env::split_paths(&path)));
    run_helper("a_config_files_helper", &repo, |command| {
        command
            .env("PATH", search.unwrap())
            .env_remove("GIT_CONFIG_NOSYSTEM")
            .env_remove("GIT_CONFIG_SYSTEM")
            .env(HELPER_INSTALLATION, &installation)
            .env(HELPER_SYSTEM, &system)
    });
}

/// A directory in `root` holding a Git whose config files lie where gix
/// would not look on its own, as Homebrew Git's do: asked as Git is, it
/// names the files [`HELPER_INSTALLATION`] and [`HELPER_SYSTEM`] name, and
/// it runs the Git `path` finds for everything else.
fn config_reporting_git(root: &Path, path: &std::ffi::OsStr) -> std::path::PathBuf {
    let real = crate::fsutil::executables::find_executable("git".as_ref(), path).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let installation =
        format!("printf 'system\\0file:%s\\0shoal.installation\\0' \"${HELPER_INSTALLATION}\"");
    let system = format!("printf '%s\\n' \"${HELPER_SYSTEM}\"");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n*--show-origin*) {installation}; exit 0;;\n*'var GIT_CONFIG_SYSTEM') {system}; exit 0;;\nesac\nexec '{}' \"$@\"\n",
        real.display()
    );
    let wrapper = bin.join("git");
    std::fs::write(&wrapper, script).unwrap();
    let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&wrapper, executable).unwrap();
    bin
}

/// Read values that only the installation and system config files set.
#[tokio::test]
#[ignore = "run by the_config_files_of_the_git_shoal_runs_are_read"]
async fn a_config_files_helper() {
    let repo = helper_repository();
    for key in ["shoal.installation", "shoal.system"] {
        let value = crate::git::config_value(&repo, key).await.unwrap();
        assert_eq!(value.as_deref(), Some("yes"), "{key}");
    }
}

#[test]
fn a_config_file_named_by_git_config_is_read_as_git_reads_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let file = root.path().join("chosen.gitconfig");
    std::fs::write(&file, "[remote]\n\tpushDefault = chosen\n").unwrap();
    run_helper("a_chosen_config_helper", &repo, |command| {
        command.env("GIT_CONFIG", &file)
    });
}

/// Read the push remote the file `GIT_CONFIG` names sets.
#[tokio::test]
#[ignore = "run by a_config_file_named_by_git_config_is_read_as_git_reads_it"]
async fn a_chosen_config_helper() {
    let repo = helper_repository();
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "chosen");
}

#[tokio::test]
async fn a_conditional_include_matches_each_checkouts_own_git_directory() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let linked = root.path().join("task");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            linked.to_str().unwrap(),
        ],
    );
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    // Git matches `gitdir` against a checkout's own Git directory: a linked
    // worktree's admin directory, the main checkout's common one.
    for (directory, value) in [
        (common.join("worktrees/task"), "linked"),
        (common.clone(), "main"),
    ] {
        let config = root.path().join(format!("{value}.gitconfig"));
        std::fs::write(&config, format!("[shoal]\n\tcheckout = {value}\n")).unwrap();
        let key = format!("includeIf.gitdir:{}.path", directory.display());
        git(&repo, &["config", &key, config.to_str().unwrap()]);
    }
    for (checkout, value) in [(&linked, "linked"), (&repo, "main")] {
        let read = crate::git::config_value(checkout, "shoal.checkout")
            .await
            .unwrap();
        assert_eq!(read.as_deref(), Some(value), "{}", checkout.display());
    }
}

#[tokio::test]
async fn a_directory_condition_both_checkouts_match_alike_reads_in_process() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let linked = root.path().join("task");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            linked.to_str().unwrap(),
        ],
    );
    let around = std::fs::canonicalize(root.path()).unwrap();
    let key = format!("includeIf.gitdir:{}/.path", around.display());
    git(&repo, &["config", &key, "/dev/null"]);
    assert_eq!(reader(&linked).await, "gix");
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    let key = format!("includeIf.gitdir:{}/worktrees/.path", common.display());
    git(&repo, &["config", &key, "/dev/null"]);
    assert_eq!(reader(&linked).await, "git");
}

/// A repository whose config includes `remote.pushDefault = fork` on a
/// condition on its remote's URL, which `url` in its config, after the
/// condition, sets: as a section of its own, or through a later include.
fn remote_conditioned(root: &Path, url: &str) -> std::path::PathBuf {
    let repo = repository(root, "repo");
    let included = root.join("included.gitconfig");
    std::fs::write(&included, "[remote]\n\tpushDefault = fork\n").unwrap();
    let config = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    let condition = "hasconfig:remote.*.url:https://example.com/**";
    text.push_str(&format!(
        "[includeIf \"{condition}\"]\n\tpath = {}\n",
        included.display()
    ));
    text.push_str(url);
    std::fs::write(&config, text).unwrap();
    repo
}

#[tokio::test]
async fn a_condition_on_a_later_remote_url_includes_as_git_includes() {
    let root = tempfile::tempdir().unwrap();
    let section = "[remote \"origin\"]\n\turl = https://example.com/app\n";
    let repo = remote_conditioned(root.path(), section);
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "fork");
}

#[tokio::test]
async fn a_condition_on_a_remote_url_a_later_include_sets_includes_as_git_includes() {
    let root = tempfile::tempdir().unwrap();
    let urls = root.path().join("urls.gitconfig");
    std::fs::write(
        &urls,
        "[remote \"origin\"]\n\turl = https://example.com/app\n",
    )
    .unwrap();
    let include = format!("[include]\n\tpath = {}\n", urls.display());
    let repo = remote_conditioned(root.path(), &include);
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "fork");
}

#[test]
fn a_condition_on_a_remote_url_the_environment_sets_includes_as_git_includes() {
    let root = tempfile::tempdir().unwrap();
    let repo = remote_conditioned(root.path(), "");
    run_helper("a_push_remote_fork_helper", &repo, |command| {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "remote.origin.url")
            .env("GIT_CONFIG_VALUE_0", "https://example.com/app")
    });
}

/// Read the push remote `fork`, which only an included file sets.
#[tokio::test]
#[ignore = "run by a_condition_on_a_remote_url_the_environment_sets_includes_as_git_includes"]
async fn a_push_remote_fork_helper() {
    let repo = helper_repository();
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "fork");
}

#[tokio::test]
async fn a_condition_on_the_branch_or_other_config_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(
        &repo,
        &["config", "includeIf.onbranch:main.path", "/dev/null"],
    );
    assert_eq!(reader(&repo).await, "git");
    git(
        &repo,
        &["config", "--remove-section", "includeIf.onbranch:main"],
    );
    assert_eq!(reader(&repo).await, "gix");
    let condition = "includeIf.hasconfig:remote.*.url:https://example.com/**.path";
    git(&repo, &["config", condition, "/dev/null"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_branch_condition_through_symbolic_refs_includes_as_git_includes() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let included = root.path().join("included.gitconfig");
    std::fs::write(&included, "[remote]\n\tpushDefault = fork\n").unwrap();
    git(
        &repo,
        &[
            "config",
            "includeIf.onbranch:main.path",
            included.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    git(&repo, &["symbolic-ref", "HEAD", "refs/heads/alias"]);
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "fork");
}

#[test]
fn a_condition_the_environment_sets_includes_as_git_includes() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(
        &repo,
        &["config", "remote.origin.url", "https://example.com/app"],
    );
    let included = root.path().join("included.gitconfig");
    std::fs::write(&included, "[remote]\n\tpushDefault = fork\n").unwrap();
    let key = "includeIf.hasconfig:remote.*.url:https://example.com/**.path";
    run_helper("a_push_remote_fork_helper", &repo, |command| {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", key)
            .env("GIT_CONFIG_VALUE_0", &included)
    });
}

#[tokio::test]
async fn files_one_include_section_names_take_precedence_as_in_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let mut section = String::from("[include]\n");
    for name in ["first", "second"] {
        let file = root.path().join(format!("{name}.gitconfig"));
        std::fs::write(&file, format!("[remote]\n\tpushDefault = {name}\n")).unwrap();
        section.push_str(&format!("\tpath = {}\n", file.display()));
    }
    let config = repo.join(".git/config");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, text + &section).unwrap();
    let expected = git(&repo, &["config", "--get", "remote.pushDefault"]);
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), expected.trim());
}

#[test]
fn a_remote_only_inherited_config_defines_is_not_the_repositorys() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let global = root.path().join("global.gitconfig");
    std::fs::write(
        &global,
        "[remote \"origin\"]\n\turl = https://example.com/global\n",
    )
    .unwrap();
    run_helper("an_inherited_remote_helper", &repo, |command| {
        command
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "remote.fork.url")
            .env("GIT_CONFIG_VALUE_0", "https://example.com/fork")
    });
}

/// Find no URL for remotes only the global config and the environment
/// define, as `git remote get-url` finds none, then one for a remote the
/// repository's config also names.
#[tokio::test]
#[ignore = "run by a_remote_only_inherited_config_defines_is_not_the_repositorys"]
async fn an_inherited_remote_helper() {
    let repo = helper_repository();
    for remote in ["origin", "fork"] {
        let url = crate::git::configured_remote_url(&repo, remote)
            .await
            .unwrap();
        assert_eq!(url, None, "{remote}");
    }
    git(
        &repo,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    let url = crate::git::configured_remote_url(&repo, "origin")
        .await
        .unwrap();
    assert_eq!(url.as_deref(), Some("https://example.com/global"));
}

#[test]
fn a_relative_config_file_is_found_where_git_finds_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    std::fs::write(
        repo.join("relative.gitconfig"),
        "[remote]\n\tpushDefault = fork\n",
    )
    .unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    // Git, run in the repository, finds the file there; this process runs
    // elsewhere.
    run_helper("a_push_remote_fork_helper", &repo, |command| {
        command
            .current_dir(&elsewhere)
            .env("GIT_CONFIG_GLOBAL", "relative.gitconfig")
    });
}
