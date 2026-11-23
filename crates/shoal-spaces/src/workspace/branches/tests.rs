use crate::git::existing_branch::BranchExt;
use crate::stand_in;
use shoal_core::test_support::{commit, git, repository};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceName;
use shoal_refs::BranchName;
use std::{fs, path::PathBuf};

use crate::{removal::InspectionPolicy, workspace::NewWorktreeOptions};
use shoal_protocol::space_names::SpaceSelector;

struct Fixture {
    repo: PathBuf,
    manager: stand_in::Fixture,
    repo_id: RepositoryId,
}

impl Fixture {
    async fn new() -> Self {
        let manager = stand_in::Fixture::new().await;
        let root = &manager.root;
        let repo = repository(root.path(), "repo with ' quotes & $literal");
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        Self {
            repo,
            manager,
            repo_id,
        }
    }

    async fn add(&self, name: &str) -> crate::model::Workspace {
        self.manager
            .create_workspace(&self.repo_id, name.into(), None, None, None)
            .await
            .unwrap()
    }

    fn remote(&self) -> PathBuf {
        let branch = git(&self.repo, &["branch", "--show-current"]);
        let branch = branch.trim_end_matches('\n');
        let origin = self.manager.root.path().join("origin.git");
        git(
            &self.repo,
            &[
                "clone",
                "--bare",
                self.repo.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        git(
            &self.repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&self.repo, &["fetch", "origin"]);
        git(
            &self.repo,
            &[
                "branch",
                &format!("--set-upstream-to=origin/{branch}"),
                branch,
            ],
        );
        let author = self.manager.root.path().join("author");
        git(
            &self.repo,
            &["clone", origin.to_str().unwrap(), author.to_str().unwrap()],
        );
        fs::write(author.join("upstream"), "remote\n").unwrap();
        commit(&author, "upstream");
        git(&author, &["push", "origin", branch]);
        author
    }
}

/// Allocate a workspace for each case's name, remove it, then open the
/// existing branch of that name: a reserved name gets a suffixed branch and
/// cannot be opened as is, and the others open at the allocated path. The
/// workspace takes the name as given, and a directory named as the case
/// says. The cases' branches and directories differ, so they share one
/// repository.
async fn check_branch_compatibility(cases: &[(String, String, bool)]) {
    let f = Fixture::new().await;
    for (name, workspace_name, reserved) in cases {
        let allocated = f.add(name).await;
        let expected = if *reserved {
            format!("{name}-2")
        } else {
            name.clone()
        };
        assert_eq!(
            allocated.branch().map(BranchName::as_str),
            Some(expected.as_str())
        );
        assert_eq!(allocated.name, SpaceName::derive(name, "workspace"));
        assert_eq!(
            allocated.path.file_name().unwrap().to_str().unwrap(),
            workspace_name
        );
        assert_eq!(
            git(&allocated.path, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{expected}\n")
        );
        f.manager
            .spaces
            .remove_selected(
                &*f.manager,
                &SpaceSelector::id(&allocated.id),
                crate::removal::UnsavedWork::All,
                InspectionPolicy::IncludeDirectoryProcesses,
            )
            .await
            .unwrap();

        let reference = format!("refs/heads/{name}");
        git(&f.repo, &["update-ref", &reference, "refs/heads/main"]);
        let before = git(&f.repo, &["rev-parse", &reference]);
        let workspaces = f.manager.spaces.list_workspaces().await.unwrap().len();
        let opened = f
            .manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, &reference, None, None, None)
            .await;
        if *reserved {
            assert!(opened.is_err(), "{name}");
            assert_eq!(
                f.manager.spaces.list_workspaces().await.unwrap().len(),
                workspaces
            );
        } else {
            let workspace = opened.unwrap().workspace;
            assert_eq!(
                workspace.branch().map(BranchName::as_str),
                Some(name.as_str())
            );
            assert_eq!(workspace.name, SpaceName::derive(name, "workspace"));
            assert_eq!(workspace.path, allocated.path);
            assert_eq!(
                git(&workspace.path, &["symbolic-ref", "HEAD"]),
                format!("{reference}\n")
            );
        }
        assert_eq!(git(&f.repo, &["rev-parse", &reference]), before);
    }
}

/// A name of `len` repetitions of `digit`, bare and nested: a bare hex name
/// of an object ID's length is reserved, and workspace names are cut to 64
/// characters.
fn digit_run(digit: &str, len: usize) -> [(String, String, bool); 2] {
    let name = digit.repeat(len);
    [
        (
            name.clone(),
            digit.repeat(len.min(64)),
            digit != "g" && matches!(len, 40 | 64),
        ),
        (
            format!("topic/{name}"),
            format!("topic-{name}").chars().take(64).collect(),
            false,
        ),
    ]
}

fn cases<const N: usize>(cases: [(&str, &str, bool); N]) -> [(String, String, bool); N] {
    cases.map(|(name, workspace_name, reserved)| (name.into(), workspace_name.into(), reserved))
}

macro_rules! branch_compatibility_tests {
    ($($test:ident: $cases:expr;)*) => {$(
        #[tokio::test]
        async fn $test() {
            check_branch_compatibility(&$cases).await;
        }
    )*};
}

// Each test covers one class of names, so the classes run in parallel.
branch_compatibility_tests! {
    branch_compatibility_reserves_head_and_at: cases([
        ("HEAD", "HEAD", true),
        ("@", "workspace", true),
    ]);
    branch_compatibility_keeps_nested_head: cases([
        ("topic/HEAD", "topic-HEAD", false),
        ("HEAD/topic", "HEAD-topic", false),
    ]);
    branch_compatibility_keeps_nested_at: cases([
        ("topic/@", "topic--", false),
        ("@/topic", "topic", false),
    ]);
    branch_compatibility_of_39_lower_hex_digits: digit_run("a", 39);
    branch_compatibility_of_39_upper_hex_digits: digit_run("B", 39);
    branch_compatibility_of_39_non_hex_letters: digit_run("g", 39);
    branch_compatibility_of_40_lower_hex_digits: digit_run("a", 40);
    branch_compatibility_of_40_upper_hex_digits: digit_run("B", 40);
    branch_compatibility_of_40_non_hex_letters: digit_run("g", 40);
    branch_compatibility_of_64_lower_hex_digits: digit_run("a", 64);
    branch_compatibility_of_64_upper_hex_digits: digit_run("B", 64);
    branch_compatibility_of_64_non_hex_letters: digit_run("g", 64);
    branch_compatibility_of_65_lower_hex_digits: digit_run("a", 65);
    branch_compatibility_of_65_upper_hex_digits: digit_run("B", 65);
    branch_compatibility_of_65_non_hex_letters: digit_run("g", 65);
}

#[tokio::test]
async fn names_suffix_only_conflicts_and_serialize_concurrent_adds() {
    let f = Fixture::new().await;
    assert_eq!(
        f.add("plain").await.branch().map(BranchName::as_str),
        Some("plain")
    );
    assert_eq!(
        f.add("HEAD").await.branch().map(BranchName::as_str),
        Some("HEAD-2")
    );
    git(&f.repo, &["branch", "feature"]);
    git(&f.repo, &["branch", "feature-2"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/feature-3", "HEAD"],
    );
    git(&f.repo, &["branch", "feature-4/nested"]);
    let workspace = f.add("feature").await;
    assert_eq!(
        workspace.branch().map(BranchName::as_str),
        Some("feature-5")
    );
    assert_eq!(workspace.path.file_name().unwrap(), "feature");
    assert_eq!(
        f.add("main").await.branch().map(BranchName::as_str),
        Some("main-2")
    );
    // Unpushed work keeps the branch, and with it the suffix.
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    f.manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert_eq!(
        f.add("feature").await.branch().map(BranchName::as_str),
        Some("feature-6")
    );
    git(&f.repo, &["branch", "concurrent"]);
    let (first, second) = tokio::join!(f.add("concurrent"), f.add("concurrent-2"));
    assert_ne!(first.branch(), second.branch());
}

#[tokio::test]
async fn a_typed_name_is_refused_rather_than_suffixed() {
    let f = Fixture::new().await;
    let typed = |name: &str| {
        let new = crate::workspace::NewWorkspace {
            exact_branch: true,
            ..Default::default()
        };
        f.manager
            .spaces
            .create_workspace_with(&*f.manager, &f.repo_id, name.into(), None, new)
    };
    assert_eq!(
        typed("free")
            .await
            .unwrap()
            .branch()
            .map(BranchName::as_str),
        Some("free")
    );
    git(&f.repo, &["branch", "taken"]);
    for name in ["taken", "taken/below", "HEAD"] {
        typed(name).await.unwrap_err();
    }
    // The two of a race take one name: one creates it, the other is refused.
    let (first, second) = tokio::join!(typed("raced"), typed("raced"));
    assert_ne!(first.is_ok(), second.is_ok());
}

#[tokio::test]
async fn local_defaults_and_unavailable_or_ambiguous_remote_defaults() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "-m", "trunk"]);
    assert_eq!(
        f.add("local").await.checkout.base_ref(),
        Some("refs/heads/trunk")
    );
    git(&f.repo, &["checkout", "--detach"]);
    assert!(
        f.manager
            .create_workspace(&f.repo_id, "detached".into(), None, None, None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(
            &f.repo_id,
            "explicit-detached".into(),
            Some("trunk".into()),
            None,
            None,
        )
        .await
        .unwrap();
    git(&f.repo, &["switch", "trunk"]);
    git(
        &f.repo,
        &[
            "remote",
            "add",
            "upstream",
            f.manager.root.path().join("missing.git").to_str().unwrap(),
        ],
    );
    assert!(
        f.manager
            .create_workspace(&f.repo_id, "offline".into(), None, None, None)
            .await
            .is_err()
    );
    f.manager
        .create_workspace(
            &f.repo_id,
            "explicit-offline".into(),
            Some("trunk".into()),
            None,
            None,
        )
        .await
        .unwrap();
    git(
        &f.repo,
        &["remote", "add", "backup", f.repo.to_str().unwrap()],
    );
    f.manager
        .create_workspace(&f.repo_id, "ambiguous".into(), None, None, None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn nested_branch_conflicts_suffix_the_blocked_component() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "henrik"]);
    git(&f.repo, &["branch", "henrik-2"]);
    git(&f.repo, &["branch", "henrik-3/topic/nested"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/henrik-3/topic-2", "HEAD"],
    );
    let workspace = f.add("henrik/topic").await;
    assert_eq!(workspace.name, "henrik/topic");
    assert_eq!(workspace.directory_name(), "henrik-topic");
    assert_eq!(
        workspace.branch().map(BranchName::as_str),
        Some("henrik-3/topic-3")
    );
    assert_eq!(
        git(&workspace.path, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/henrik-3/topic-3"
    );
    // A sibling can share an existing namespace without renaming its prefix.
    assert_eq!(
        f.add("henrik-3/other")
            .await
            .branch()
            .map(BranchName::as_str),
        Some("henrik-3/other")
    );
}

#[tokio::test]
async fn branch_selection_rejects_revision_shorthand() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "-c", "previous"]);
    git(&f.repo, &["switch", "main"]);
    for name in ["@{-1}", "main~0", "main^0", "@{0}"] {
        assert!(
            f.manager
                .spaces
                .open_branch(&*f.manager, &f.repo_id, name, None, None, None)
                .await
                .is_err(),
            "{name}"
        );
    }
    assert!(f.manager.spaces.list_workspaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn existing_branch_opens_without_suffix_and_reuses_owned_workspace() {
    let f = Fixture::new().await;
    git(&f.repo, &["branch", "coworker/topic"]);
    let opened = f
        .manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "coworker/topic", None, None, None)
        .await
        .unwrap();
    assert!(!opened.reused);
    assert_eq!(
        opened.workspace.branch().map(BranchName::as_str),
        Some("coworker/topic")
    );
    assert_eq!(opened.workspace.name, "coworker/topic");
    assert_eq!(opened.workspace.directory_name(), "coworker-topic");
    assert_eq!(
        opened.workspace.checkout.base_ref(),
        Some("refs/heads/main")
    );
    assert_eq!(
        git(&opened.workspace.path, &["branch", "--show-current"]).trim(),
        "coworker/topic"
    );
    let again = f
        .manager
        .spaces
        .open_branch(
            &*f.manager,
            &f.repo_id,
            "refs/heads/coworker/topic",
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.workspace.id, opened.workspace.id);
    assert_eq!(f.manager.spaces.usage.activity(&opened.workspace.id), 1);
    git(&opened.workspace.path, &["switch", "--detach"]);
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "coworker/topic", None, None, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn existing_branch_records_an_explicit_base_for_new_worktrees() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "-c", "stack/base"]);
    fs::write(f.repo.join("base"), "base\n").unwrap();
    commit(&f.repo, "base");
    git(&f.repo, &["switch", "-c", "stack/top"]);
    fs::write(f.repo.join("top"), "top\n").unwrap();
    commit(&f.repo, "top");
    git(&f.repo, &["switch", "main"]);
    let opened = f
        .manager
        .spaces
        .open_branch(
            &*f.manager,
            &f.repo_id,
            "stack/top",
            None,
            None,
            Some("stack/base".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        opened.workspace.checkout.base_ref(),
        Some("refs/heads/stack/base")
    );
    assert_eq!(
        f.manager
            .spaces
            .diff_base(&*f.manager, &SpaceSelector::id(&opened.workspace.id))
            .await
            .unwrap()
            .commit,
        git(&f.repo, &["rev-parse", "stack/base"]).trim()
    );
    f.manager
        .spaces
        .open_branch(
            &*f.manager,
            &f.repo_id,
            "stack/top",
            None,
            None,
            Some("main".into()),
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn existing_branch_refuses_other_checkouts_and_takes_a_free_directory() {
    let f = Fixture::new().await;
    f.manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "main", None, None, None)
        .await
        .unwrap_err();
    assert!(f.manager.spaces.list_workspaces().await.unwrap().is_empty());
    git(&f.repo, &["branch", "topic/one"]);
    f.add("topic-one").await;
    // Its directory would be another workspace's, so it takes a suffix.
    let opened = f
        .manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "topic/one", None, None, None)
        .await
        .unwrap();
    assert_eq!(opened.workspace.name, "topic/one");
    assert_eq!(opened.workspace.directory_name(), "topic-one-2");
    git(&f.repo, &["switch", "--detach"]);
    let main = f
        .manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "main", None, None, None)
        .await
        .unwrap();
    assert_eq!(
        main.workspace.branch().map(BranchName::as_str),
        Some("main")
    );
    f.manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&main.workspace.id),
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!main.workspace.path.exists());
    assert_eq!(
        git(&f.repo, &["rev-parse", "main"]),
        git(&f.repo, &["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn creation_check_asks_the_remote_for_a_new_branch_base_only() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["push", "origin", "HEAD:refs/heads/unfetched"]);
    // A tracking ref the remote no longer has.
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/stale", "HEAD"],
    );
    git(&f.repo, &["branch", "topic"]);
    let base = |base| NewWorktreeOptions {
        base: Some(base),
        ..NewWorktreeOptions::default()
    };
    for (branch, existing, options) in [
        (Some("new"), None, base("origin/unfetched")),
        (None, None, base("origin/unfetched")),
        (None, Some("topic"), base("origin/main")),
        // An unsettled branch may still open a local branch, which uses the
        // stale ref without fetching.
        (None, None, base("origin/stale")),
        (None, Some("topic"), base("origin/stale")),
        // Opening a remote branch fetches and creates it before resolving the
        // base, which may name it in any form.
        (None, Some("origin/unfetched"), base("origin/unfetched")),
        (None, Some("origin/unfetched"), base("unfetched~1")),
    ] {
        f.manager
            .spaces
            .check_creation(&*f.manager, &f.repo_id, branch, existing, options)
            .await
            .unwrap();
    }
    for (branch, existing, options) in [
        (Some("new"), None, base("origin/missing")),
        (Some("new"), None, base("origin/stale")),
        (None, None, base("origin/missing")),
        // Opening an existing branch compares against the base without fetching it.
        (None, Some("topic"), base("origin/unfetched")),
    ] {
        f.manager
            .spaces
            .check_creation(&*f.manager, &f.repo_id, branch, existing, options)
            .await
            .unwrap_err();
    }
}

#[tokio::test]
async fn existing_remote_branch_discovers_fetches_and_tracks_new_heads() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "coworker/topic"]);
    fs::write(author.join("coworker"), "work\n").unwrap();
    commit(&author, "coworker");
    git(&author, &["push", "origin", "coworker/topic"]);
    let branches = f
        .manager
        .spaces
        .branches(&*f.manager, &f.repo_id)
        .await
        .unwrap();
    assert!(
        branches
            .iter()
            .any(|b| b.selector() == "refs/remotes/origin/coworker/topic")
    );
    let opened = f
        .manager
        .spaces
        .open_branch(
            &*f.manager,
            &f.repo_id,
            "origin/coworker/topic",
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        opened.workspace.branch().map(BranchName::as_str),
        Some("coworker/topic")
    );
    assert_eq!(
        git(&opened.workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(
            &opened.workspace.path,
            &["rev-parse", "--symbolic-full-name", "@{upstream}"]
        )
        .trim(),
        "refs/remotes/origin/coworker/topic"
    );
    assert!(
        f.manager
            .spaces
            .open_branch(
                &*f.manager,
                &f.repo_id,
                "origin/coworker/topic",
                None,
                None,
                None
            )
            .await
            .unwrap()
            .reused
    );
}

#[tokio::test]
async fn existing_remote_branch_rejects_ambiguity_and_unrelated_local_branch() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    let origin = f.manager.root.path().join("origin.git");
    git(
        &f.repo,
        &["remote", "add", "other", origin.to_str().unwrap()],
    );
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "topic", None, None, None)
            .await
            .is_err()
    );
    git(&f.repo, &["branch", "topic"]);
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "origin/topic", None, None, None)
            .await
            .is_err()
    );
    assert!(f.manager.spaces.list_workspaces().await.unwrap().is_empty());
    // Explicit local selection remains usable with unavailable remotes.
    git(
        &f.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/shoal-test-remote",
        ],
    );
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "topic", None, None, None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn existing_tracking_branch_fast_forwards_and_refuses_divergence() {
    let f = Fixture::new().await;
    let author = f.remote();
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    git(&f.repo, &["fetch", "origin"]);
    git(&f.repo, &["branch", "--track", "topic", "origin/topic"]);
    fs::write(author.join("later"), "new work\n").unwrap();
    commit(&author, "later");
    git(&author, &["push", "origin", "topic"]);
    let opened = f
        .manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "origin/topic", None, None, None)
        .await
        .unwrap();
    assert_eq!(
        git(&opened.workspace.path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    git(&author, &["switch", "-c", "diverged"]);
    git(&author, &["push", "origin", "diverged"]);
    git(&f.repo, &["fetch", "origin"]);
    git(&f.repo, &["switch", "--track", "origin/diverged"]);
    fs::write(f.repo.join("local"), "local work\n").unwrap();
    commit(&f.repo, "local");
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["switch", "main"]);
    fs::write(author.join("remote"), "remote work\n").unwrap();
    commit(&author, "remote");
    git(&author, &["push", "origin", "diverged"]);
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "origin/diverged", None, None, None)
            .await
            .is_err()
    );
    assert_eq!(git(&f.repo, &["rev-parse", "diverged"]), before);
}

#[tokio::test]
async fn existing_default_branch_survives_normal_workspace_removal() {
    let f = Fixture::new().await;
    f.remote();
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&f.repo, &["switch", "--detach"]);
    let opened = f
        .manager
        .spaces
        .open_branch(&*f.manager, &f.repo_id, "main", None, None, None)
        .await
        .unwrap();
    let opening = git(&f.repo, &["rev-parse", "main"]);
    assert!(opened.workspace.checkout.base_ref().is_none());
    fs::write(
        opened.workspace.path.join("new-work"),
        "workspace changes\n",
    )
    .unwrap();
    commit(&opened.workspace.path, "new-work");
    let diff = f
        .manager
        .spaces
        .diff_base(&*f.manager, &SpaceSelector::id(&opened.workspace.id))
        .await
        .unwrap();
    assert_eq!(diff.commit, opening.trim());
    assert_eq!(
        git(
            &opened.workspace.path,
            &["diff", "--name-only", diff.commit.as_str()]
        )
        .trim(),
        "new-work"
    );
    let before = git(&f.repo, &["rev-parse", "main"]);
    let removed = f
        .manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&opened.workspace.id),
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted());
    assert!(!opened.workspace.path.exists());
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
    assert!(
        f.manager
            .spaces
            .open_branch(&*f.manager, &f.repo_id, "main", None, None, None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn copy_adoption_records_ownership_and_finishes_ready() {
    let f = Fixture::new().await;
    let source = f.add("source").await;
    fs::write(source.path.join("copied"), "copied files\n").unwrap();
    let copied = f
        .manager
        .spaces
        .adopt_workspace(&*f.manager, &f.repo_id, &source.path, true)
        .await
        .unwrap();

    assert_eq!(copied.branch().map(BranchName::as_str), Some("source-2"));
    assert_eq!(copied.state, crate::state::WorkspaceState::Ready);
    assert_eq!(
        fs::read_to_string(copied.path.join("copied")).unwrap(),
        "copied files\n"
    );
    assert!(copied.checkout.git_dir().is_some());
    assert_eq!(
        git(&f.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        3
    );
}

#[tokio::test]
async fn adoption_replaces_markers_of_forgotten_workspaces_only() {
    let f = Fixture::new().await;
    let marked = |branch: &str, owner: &str| {
        let path = f.manager.root.path().join(branch.replace('/', "-"));
        git(
            &f.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        let marker =
            PathBuf::from(git(&path, &["rev-parse", "--path-format=absolute", "--git-dir"]).trim())
                .join("shoal-workspace");
        fs::write(&marker, format!("{owner}\n")).unwrap();
        (path, marker)
    };
    // A marker outliving its record, e.g. after a state reset, is replaced.
    let (path, marker) = marked("adopt/stale", "forgotten-workspace");
    let w = f
        .manager
        .spaces
        .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", w.id));
    // It reports the number it took.
    assert_eq!(
        w.number,
        f.manager
            .spaces
            .workspace_by_id(&w.id)
            .await
            .unwrap()
            .number
    );
    assert_ne!(w.number, 0);
    // One naming a recorded workspace refuses adoption and stays in place.
    let (path, marker) = marked("adopt/owned", w.id.as_str());
    f.manager
        .spaces
        .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
        .await
        .unwrap_err();
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", w.id));
}

#[tokio::test]
async fn adoption_refuses_invalid_roots_and_moved_owned_worktrees() {
    let f = Fixture::new().await;
    assert!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &f.repo, false)
            .await
            .is_err()
    );
    let path = f.manager.root.path().join("external");
    git(
        &f.repo,
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    );
    assert!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
            .await
            .is_err()
    );
    git(&path, &["switch", "-c", "adopt/topic"]);
    git(&f.repo, &["worktree", "lock", path.to_str().unwrap()]);
    assert!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
            .await
            .is_err()
    );
    git(&f.repo, &["worktree", "unlock", path.to_str().unwrap()]);
    fs::create_dir(path.join("child")).unwrap();
    assert!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &path.join("child"), false)
            .await
            .is_err()
    );
    let other = Fixture::new().await;
    assert!(
        other
            .manager
            .spaces
            .adopt_workspace(&*other.manager, &other.repo_id, &path, false)
            .await
            .is_err()
    );
    let owned = f.add("owned").await;
    let moved = f.manager.root.path().join("moved");
    git(
        &f.repo,
        &[
            "worktree",
            "move",
            owned.path.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    git(&moved, &["switch", "-c", "renamed"]);
    assert!(
        f.manager
            .spaces
            .adopt_workspace(&*f.manager, &f.repo_id, &moved, false)
            .await
            .is_err()
    );
    assert_eq!(f.manager.spaces.list_workspaces().await.unwrap().len(), 1);
    assert!(path.join("tracked").exists());
}

#[tokio::test]
async fn adoption_of_default_branch_uses_opening_commit_and_retains_branch() {
    let f = Fixture::new().await;
    git(&f.repo, &["switch", "--detach"]);
    let path = f.manager.root.path().join("external");
    git(
        &f.repo,
        &["worktree", "add", path.to_str().unwrap(), "main"],
    );
    // A symbolic remote HEAD lets default discovery work from the detached main checkout.
    git(
        &f.repo,
        &["remote", "add", "origin", "https://example.invalid/repo"],
    );
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let w = f
        .manager
        .spaces
        .adopt_workspace(&*f.manager, &f.repo_id, &path, false)
        .await
        .unwrap();
    assert!(w.checkout.base_ref().is_none());
    assert_eq!(
        w.checkout.base().map(|base| base.commit.as_str()),
        Some(git(&path, &["rev-parse", "HEAD"]).trim())
    );
    f.manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&w.id),
            crate::removal::UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(
        !git(&f.repo, &["branch", "--list", "main"])
            .trim()
            .is_empty()
    );
}
