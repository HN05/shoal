use super::*;

#[test]
fn stats_add_up_every_container_in_docker_or_podman_format() {
    let docker = concat!(
        r#"{"CPUPerc":"12.50%","ID":"a1","MemUsage":"1.5GiB / 7.6GiB","Name":"shoal-w-db-1"}"#,
        "\n",
        r#"{"CPUPerc":"--","ID":"b2","MemUsage":"512KiB / 7.6GiB","Name":"shoal-w-cache-1"}"#,
        "\n",
    );
    assert_eq!(
        parse_stats(docker).unwrap(),
        ContainerUsage {
            cpu_percent: 12.5,
            memory_bytes: (3 << 29) + (1 << 19),
        }
    );
    let podman = r#"[{"id":"a1","name":"db","cpu_percent":"150.25%","mem_usage":"2.5MB / 8GB"},
        {"id":"b2","name":"cache","cpu_percent":"0.75%","mem_usage":"1kB / 8GB"}]"#;
    assert_eq!(
        parse_stats(podman).unwrap(),
        ContainerUsage {
            cpu_percent: 151.0,
            memory_bytes: 2_501_000,
        }
    );
}

#[test]
fn a_reading_that_does_not_parse_fails_the_project() {
    let unknown = r#"{"CPUPerc":"1%","MemUsage":"3 parsecs / 1GiB"}"#;
    assert!(parse_stats(unknown).is_err());
    assert!(parse_stats(r#"{"CPUPerc":"lots","MemUsage":"1B / 1GiB"}"#).is_err());
}
