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

/// The variable that names the installation config
/// [`an_installation_config_helper`] reads.
const HELPER_INSTALLATION: &str = "SHOAL_TEST_INSTALLATION_CONFIG";

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
fn the_installation_config_of_the_git_shoal_runs_is_read() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let config = root.path().join("installation.gitconfig");
    std::fs::write(&config, "[remote]\n\tpushDefault = fork\n").unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let bin = installation_reporting_git(root.path(), &path);
    let search = std::env::join_paths([bin].into_iter().chain(std::env::split_paths(&path)));
    run_helper("an_installation_config_helper", &repo, |command| {
        command
            .env("PATH", search.unwrap())
            .env_remove("GIT_CONFIG_NOSYSTEM")
            .env_remove("GIT_CONFIG_SYSTEM")
            .env(HELPER_INSTALLATION, &config)
    });
}

/// A directory in `root` holding a Git whose installation config lies where
/// gix would not look on its own, as Homebrew Git's does: it names the file
/// [`HELPER_INSTALLATION`] names when asked, as Git does, and runs the Git
/// `path` finds for everything else.
fn installation_reporting_git(root: &Path, path: &std::ffi::OsStr) -> std::path::PathBuf {
    let real = crate::fsutil::executables::find_executable("git".as_ref(), path).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let report =
        format!("printf 'system\\0file:%s\\0remote.pushdefault\\0' \"${HELPER_INSTALLATION}\"");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *--show-origin*) {report}; exit 0;; esac\nexec '{}' \"$@\"\n",
        real.display()
    );
    let wrapper = bin.join("git");
    std::fs::write(&wrapper, script).unwrap();
    let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&wrapper, executable).unwrap();
    bin
}

/// Read a push remote that only the installation config sets.
#[tokio::test]
#[ignore = "run by the_installation_config_of_the_git_shoal_runs_is_read"]
async fn an_installation_config_helper() {
    let repo = helper_repository();
    let remote = crate::git::push_remote(&repo, &"topic".into()).await;
    assert_eq!(remote.unwrap(), "fork");
}
