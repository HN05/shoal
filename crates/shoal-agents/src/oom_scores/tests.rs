use super::*;

fn process(pid: u32) -> identity::Identity {
    identity::Identity {
        pid,
        birth: format!("boot:{pid}"),
        unique: None,
    }
}

/// An entry whose execution `id` runs `child` under the wrapper `wrapper`.
fn entry(id: &str, wrapper: u32, child: Option<identity::Identity>, reach: Reach) -> Scored {
    let execution = serde_json::json!({
        "id": id, "space_id": "space", "state": "running",
        "wrapper": process(wrapper), "child": child, "group_id": null,
    });
    Scored {
        execution: Some(serde_json::from_value(execution).unwrap()),
        reach,
    }
}

#[test]
fn each_entrys_processes_take_its_place_and_every_wrapper_the_floor() {
    let entries = [
        entry("demo", 10, None, Reach::Marked),
        Scored {
            execution: None,
            reach: Reach::JobScope,
        },
        entry("agent", 20, None, Reach::Marked),
    ];
    let marked = [
        ("demo".into(), process(10)),
        ("demo".into(), process(11)),
        ("agent".into(), process(20)),
        ("agent".into(), process(21)),
        ("agent".into(), process(22)),
        // A wrapper the agent's command started, carrying its marker.
        ("agent".into(), process(23)),
    ];
    let wrappers: Wrappers = [10, 20, 23].map(|pid| (pid, process(pid).birth)).into();
    let (wanted, failed) = wanted_scores(&entries, &marked, &wrappers, 100);
    assert!(failed.is_none());
    let mut wanted: Vec<_> = (wanted.into_iter())
        .map(|(found, score)| (found.pid(), score))
        .collect();
    wanted.sort();
    // The nested wrapper returns to the floor from the agent's score.
    assert_eq!(
        wanted,
        [
            (10, 100),
            (11, 900),
            (20, 100),
            (21, 200),
            (22, 200),
            (23, 100)
        ]
    );
}

#[test]
fn a_seed_build_s_step_takes_the_place_of_its_build() {
    use std::os::unix::process::CommandExt as _;
    let mut step = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let seed = Scored {
        execution: None,
        reach: Reach::Group(step.id()),
    };
    let entries = [seed, entry("agent", 20, None, Reach::Marked)];
    let (wanted, _) = wanted_scores(&entries, &[], &Wrappers::new(), 100);
    let wanted: Vec<_> = (wanted.into_iter())
        .map(|(found, score)| (found.pid(), score))
        .collect();
    step.kill().unwrap();
    step.wait().unwrap();
    assert_eq!(wanted, [(step.id(), 900)]);
}

#[test]
fn a_command_inside_a_command_takes_the_place_of_the_enclosing_one() {
    // The enclosing command's child, a shell, runs the inner command, which
    // carries its own execution's marker.
    let mut shell = std::process::Command::new("sh")
        .args(["-c", "sleep 30; :"])
        .spawn()
        .unwrap();
    let child = identity::capture(shell.id()).unwrap().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let inner = loop {
        let tree = identity::tree_processes(shell.id()).unwrap();
        if let Some(inner) = tree.into_iter().find(|process| process.pid != shell.id()) {
            break inner;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the inner command started"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let entries = [entry("command", 10, Some(child), Reach::ChildTree)];
    let (wanted, _) = wanted_scores(&entries, &[], &Wrappers::new(), 100);
    let mut wanted: Vec<_> = (wanted.into_iter())
        .map(|(found, score)| (found.pid(), score))
        .collect();
    wanted.sort();
    identity::signal(&inner, libc::SIGKILL).unwrap();
    shell.kill().unwrap();
    shell.wait().unwrap();
    let mut expected = [(shell.id(), 900), (inner.pid, 900)];
    expected.sort();
    assert_eq!(wanted, expected);
}
