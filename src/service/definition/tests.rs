use super::*;

fn paths() -> Paths {
    Paths::for_test("/tmp/a & b/%name/$data")
}

#[test]
fn launch_agent_preserves_literal_arguments() {
    let definition = definition(&paths(), Path::new("/tmp/a & b/shoal"), Platform::Mac).unwrap();
    let value = plist::Value::from_reader_xml(definition.content.as_bytes()).unwrap();
    assert_eq!(
        value.as_dictionary().unwrap()["ExitTimeOut"].as_unsigned_integer(),
        Some(SERVICE_STOP_TIMEOUT.as_secs())
    );
    let args = value.as_dictionary().unwrap()["ProgramArguments"]
        .as_array()
        .unwrap();
    assert_eq!(args[0].as_string(), Some("/tmp/a & b/shoal"));
    assert_eq!(args[2].as_string(), Some("/tmp/a & b/%name/$data/state"));
}

#[test]
fn systemd_escapes_expansion_and_quotes() {
    assert_eq!(
        systemd_quote("/a %n/$HOME/\"b", true).unwrap(),
        "\"/a %%n/$$HOME/\\\"b\""
    );
    assert!(systemd_quote("/a\nExecStart=bad", true).is_err());
    let definition = definition(&paths(), Path::new("/tmp/shoal"), Platform::Linux).unwrap();
    assert!(definition.content.contains("Type=exec"));
    assert!(
        definition
            .content
            .lines()
            .any(|line| { line == format!("TimeoutStopSec={}", SERVICE_STOP_TIMEOUT.as_secs()) })
    );
    assert!(definition.content.contains("%%name/$$data"));
}
