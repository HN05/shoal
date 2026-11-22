use super::*;

fn process(pid: u32) -> identity::Identity {
    identity::Identity {
        pid,
        birth: format!("boot:{pid}"),
        unique: None,
    }
}

/// An entry whose execution `id` runs under the wrapper `wrapper`.
fn entry(id: &str, wrapper: u32, job: bool) -> Scored {
    let execution = serde_json::json!({
        "id": id, "space_id": "space", "state": "running",
        "wrapper": process(wrapper), "child": null, "group_id": null,
    });
    Scored {
        execution: Some(serde_json::from_value(execution).unwrap()),
        job,
    }
}

#[test]
fn each_entrys_processes_take_its_place_and_every_wrapper_the_floor() {
    let entries = [
        entry("demo", 10, false),
        Scored {
            execution: None,
            job: true,
        },
        entry("agent", 20, false),
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
