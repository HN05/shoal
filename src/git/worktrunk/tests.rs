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

/// Exercise the real adapter against the CI-pinned Worktrunk (0.78.0).
/// Its selector expands @; its Git worktree command interprets full object
/// IDs according to the repository's object format. Other hex lengths and
/// nested components remain literal. A successful exit alone is insufficient:
/// HEAD can create a detached worktree instead of opening refs/heads/HEAD.
#[tokio::test]
async fn literal_branch_compatibility_with_worktrunk() {
    for (format, oid_len) in [("sha1", 40), ("sha256", 64)] {
        let mut cases = vec![("HEAD".to_owned(), false), ("@".to_owned(), false)];
        for len in [39, 40, 41, 63, 64, 65] {
            for digit in ["a", "B", "g"] {
                cases.push((digit.repeat(len), digit == "g" || len != oid_len));
            }
        }
        for (name, literal) in cases {
            for (name, literal) in [
                (name.clone(), literal),
                (format!("topic/{name}"), true),
                (format!("{name}/topic"), true),
            ] {
                for existing in [false, true] {
                    let root = tempfile::tempdir().unwrap();
                    let repo = root.path().join("repo");
                    std::fs::create_dir(&repo).unwrap();
                    git(
                        &repo,
                        &["init", "-b", "main", &format!("--object-format={format}")],
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
                        &name,
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
                        "{format} {name} existing={existing}: {result:?}, {head:?}"
                    );
                }
            }
        }
    }
}
