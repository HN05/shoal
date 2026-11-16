use super::*;

fn reading(id: &str, cpu_percent: f64, memory_bytes: u64) -> Reading {
    Reading {
        id: id.into(),
        usage: ContainerUsage {
            cpu_percent,
            memory_bytes,
        },
    }
}

#[test]
fn stats_read_every_container_in_docker_or_podman_format() {
    let docker = concat!(
        r#"{"CPUPerc":"12.50%","ID":"a1","MemUsage":"1.5GiB / 7.6GiB","Name":"shoal-w-db-1"}"#,
        "\n",
        r#"{"CPUPerc":"--","ID":"b2","MemUsage":"512KiB / 7.6GiB","Name":"shoal-w-cache-1"}"#,
        "\n",
    );
    assert_eq!(
        parse_stats(docker).unwrap(),
        [reading("a1", 12.5, 3 << 29), reading("b2", 0.0, 1 << 19)]
    );
    let podman = r#"[{"id":"a1","name":"db","cpu_percent":"150.25%","mem_usage":"2.5MB / 8GB"},
        {"id":"b2","name":"cache","cpu_percent":"0.75%","mem_usage":"1kB / 8GB"}]"#;
    assert_eq!(
        parse_stats(podman).unwrap(),
        [reading("a1", 150.25, 2_500_000), reading("b2", 0.75, 1000)]
    );
}

#[test]
fn a_reading_that_does_not_parse_fails_the_runtime() {
    let unknown = r#"{"ID":"a1","CPUPerc":"1%","MemUsage":"3 parsecs / 1GiB"}"#;
    assert!(parse_stats(unknown).is_err());
    assert!(parse_stats(r#"{"ID":"a1","CPUPerc":"lots","MemUsage":"1B / 1GiB"}"#).is_err());
}

#[test]
fn a_project_adds_up_the_readings_of_its_containers_by_their_shortened_ids() {
    let readings = [
        reading("aaaa", 10.0, 100),
        reading("bbbb", 5.0, 50),
        reading("cccc", 1.0, 10),
        reading("", 7.0, 70),
    ];
    let containers = ["aaaa1111".to_owned(), "bbbb2222".to_owned()];
    assert_eq!(
        project_usage(&containers, &readings),
        ContainerUsage {
            cpu_percent: 15.0,
            memory_bytes: 150,
        }
    );
    assert_eq!(project_usage(&[], &readings), ContainerUsage::default());
}
