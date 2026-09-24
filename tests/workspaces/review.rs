use crate::support::{Fixture, git, upstream_remote};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn review_runs_the_configured_command_or_prompts_an_agent() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreview = ['printf', 'manual %s', '{branch}']\nreviewer = ['printf', '%s|', '{prompt}', '{args}']\n",
    ));
    fixture.add("changes");
    let output = fixture.run(&["review", "changes"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--manual or --agent"),
        "{output:?}"
    );
    assert_eq!(
        fixture.run(&["review", "changes", "--manual"]).stdout,
        b"manual changes"
    );
    for args in [
        &["review", "changes", "--agent", "reviewer", "--", "extra"][..],
        &["run", "reviewer", "changes", "--", "extra"][..],
    ] {
        let output = fixture.run(args);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.ends_with("|extra|"), "{stdout}");
        if args[0] == "review" {
            assert!(stdout.contains("Review the changes on branch changes"));
            assert!(stdout.contains("`shoal diff`"));
        }
    }
    // Without a configured review command, the default agent reviews.
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    fixture.add("agent-only");
    let output = fixture.run(&["review", "agent-only"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("branch agent-only"));
}

#[test]
fn pr_review_opens_the_head_against_its_base_and_reuses_the_owner() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'reviewer'\n[commands]\nreviewer = ['printf', '%s', '{prompt}']\n",
    ));
    let author = upstream_remote(&fixture);
    // Serve the local bare origin over a forge-shaped ssh URL.
    let remote = fixture.root.path().join("origin.git");
    let ssh = fixture.root.path().join("ssh");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$(printf '%s' \"$last\" | sed 's|/team/project.git|{}|')\"\n",
            remote.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.repo,
        &["config", "core.sshCommand", ssh.to_str().unwrap()],
    );
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@forge.example/team/project.git",
        ],
    );
    let commit = |message: &str| {
        fs::write(author.join(message), "change\n").unwrap();
        git(&author, &["add", message]);
        git(
            &author,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                message,
            ],
        );
    };
    git(&author, &["switch", "-c", "stack/base"]);
    commit("base");
    git(&author, &["switch", "-c", "stack/top"]);
    commit("top");
    git(&author, &["push", "origin", "stack/base", "stack/top"]);

    let bin = fixture.root.path().join("pr-bin");
    fs::create_dir(&bin).unwrap();
    let fj_args = fixture.root.path().join("fj-args");
    fs::write(
        bin.join("fj"),
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf 'Top change #7\\nBy user — Open — +1 -0\\nFrom `stack/top` into `stack/base`\\n'\n",
            fj_args.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("fj"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let review = |input: &str| {
        fixture
            .command()
            .args(["pr", "review", input, "--agent", "reviewer"])
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let output = review("7");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "Review pull request #7: Top change\nhttps://forge.example/team/project/pulls/7"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("branch stack/top"), "{stdout}");
    assert!(
        fs::read_to_string(&fj_args)
            .unwrap()
            .contains("pr\0view\x007\0--host\0forge.example\0")
    );
    let workspace = &fixture.ok(&["inspect", "stack-top"])["workspace"];
    assert_eq!(workspace["base_ref"], "refs/remotes/origin/stack/base");
    let path_in = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        git(
            path_in,
            &["merge-base", "--fork-point", "origin/stack/base", "HEAD"]
        ),
        git(&author, &["rev-parse", "stack/base"])
    );

    let output = review("https://forge.example/team/project/pulls/7");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Reviewing PR #7 in workspace stack-top"),
        "{output:?}"
    );
    let output = review("https://forge.example/other/project/pulls/7");
    assert!(!output.status.success());
}
