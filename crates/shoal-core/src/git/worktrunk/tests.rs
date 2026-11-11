use super::*;

mod boundary;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// An object format and the length of its full object IDs.
struct ObjectFormat {
    name: &'static str,
    oid_len: usize,
}

const SHA1: ObjectFormat = ObjectFormat {
    name: "sha1",
    oid_len: 40,
};
const SHA256: ObjectFormat = ObjectFormat {
    name: "sha256",
    oid_len: 64,
};

/// Exercise the real adapter against the CI-pinned Worktrunk (0.81.0).
/// Its selector expands @; its Git worktree command interprets full object
/// IDs according to the repository's object format. Other hex lengths and
/// nested components remain literal. A successful exit alone is insufficient:
/// HEAD can create a detached worktree instead of opening refs/heads/HEAD.
/// Each test covers one class of names in one object format, so the classes
/// run in parallel.
async fn check_literal_branches(format: &ObjectFormat, names: &[(String, bool)]) {
    for (name, literal) in names {
        for (name, literal) in [
            (name.clone(), *literal),
            (format!("topic/{name}"), true),
            (format!("{name}/topic"), true),
        ] {
            for existing in [false, true] {
                check_literal_branch(format, &name, existing, literal).await;
            }
        }
    }
}

/// Open `name` with Worktrunk in a new repository of `format`, where the
/// branch already exists when `existing`, and check whether it opened the
/// branch of that literal name.
async fn check_literal_branch(format: &ObjectFormat, name: &str, existing: bool, literal: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(
        &repo,
        &[
            "init",
            "-b",
            "main",
            &format!("--object-format={}", format.name),
        ],
    );
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    if existing {
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/heads/{name}"),
                "refs/heads/main",
            ],
        );
    }
    let config = root.path().join("worktrunk.toml");
    std::fs::write(&config, "").unwrap();
    let workspace = root.path().join("workspace");
    let result = create(
        &repo,
        &config,
        &workspace,
        name,
        (!existing).then_some("refs/heads/main"),
    )
    .await;
    let head = crate::git::run(&workspace, &["symbolic-ref", "HEAD"]).await;
    let opened_literal = result.is_ok()
        && head
            .as_deref()
            .is_ok_and(|head| head == format!("refs/heads/{name}\n"));
    assert_eq!(
        opened_literal, literal,
        "{} {name} existing={existing}: {result:?}, {head:?}",
        format.name
    );
}

/// HEAD and @, which Worktrunk never opens as branches of those names.
fn head_and_at() -> Vec<(String, bool)> {
    vec![("HEAD".to_owned(), false), ("@".to_owned(), false)]
}

/// Names of `len` repeated hex and non-hex digits, which are literal unless
/// they have the length of `format`'s object IDs and are hex.
fn digit_runs(format: &ObjectFormat, len: usize) -> Vec<(String, bool)> {
    ["a", "B", "g"]
        .into_iter()
        .map(|digit| (digit.repeat(len), digit == "g" || len != format.oid_len))
        .collect()
}

macro_rules! literal_branch_tests {
    ($($test:ident: $format:expr, $names:expr;)*) => {$(
        #[tokio::test]
        async fn $test() {
            check_literal_branches(&$format, &$names).await;
        }
    )*};
}

literal_branch_tests! {
    sha1_head_and_at_are_not_literal: SHA1, head_and_at();
    sha1_39_digit_names: SHA1, digit_runs(&SHA1, 39);
    sha1_40_digit_names: SHA1, digit_runs(&SHA1, 40);
    sha1_41_digit_names: SHA1, digit_runs(&SHA1, 41);
    sha1_63_digit_names: SHA1, digit_runs(&SHA1, 63);
    sha1_64_digit_names: SHA1, digit_runs(&SHA1, 64);
    sha1_65_digit_names: SHA1, digit_runs(&SHA1, 65);
    sha256_head_and_at_are_not_literal: SHA256, head_and_at();
    sha256_39_digit_names: SHA256, digit_runs(&SHA256, 39);
    sha256_40_digit_names: SHA256, digit_runs(&SHA256, 40);
    sha256_41_digit_names: SHA256, digit_runs(&SHA256, 41);
    sha256_63_digit_names: SHA256, digit_runs(&SHA256, 63);
    sha256_64_digit_names: SHA256, digit_runs(&SHA256, 64);
    sha256_65_digit_names: SHA256, digit_runs(&SHA256, 65);
}

/// Template syntax in the workspace path stays literal, and the default branch
/// opens at that path instead of being routed to the main checkout.
#[tokio::test]
async fn default_branch_opens_at_a_literal_path() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    git(&repo, &["switch", "-c", "other"]);
    let config = root.path().join("worktrunk.toml");
    std::fs::write(&config, "").unwrap();
    let workspace = root.path().join("main {{ branch }}");
    create(&repo, &config, &workspace, "main", None)
        .await
        .unwrap();
    let head = crate::git::run(&workspace, &["symbolic-ref", "HEAD"]).await;
    assert_eq!(head.unwrap(), "refs/heads/main\n");
}

/// The copy reports how many files it brought over, and none once they exist.
#[tokio::test]
async fn copy_ignored_counts_the_files_it_copies() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    std::fs::write(repo.join(".env"), "KEY=1\n").unwrap();
    let config = root.path().join("worktrunk.toml");
    std::fs::write(&config, "").unwrap();
    let workspace = root.path().join("topic");
    create(&repo, &config, &workspace, "topic", Some("main"))
        .await
        .unwrap();
    assert_eq!(copy_ignored(&config, &workspace).await.unwrap(), 1);
    assert_eq!(copy_ignored(&config, &workspace).await.unwrap(), 0);
    assert_eq!(
        std::fs::read_to_string(workspace.join(".env")).unwrap(),
        "KEY=1\n"
    );
}
