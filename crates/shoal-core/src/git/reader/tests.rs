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
