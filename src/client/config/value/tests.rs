use super::*;

#[derive(Debug, Deserialize)]
struct Setting {
    value: Value,
}

fn parse(text: &str) -> Result<Value, toml::de::Error> {
    toml::from_str::<Setting>(text).map(|setting| setting.value)
}

#[tokio::test]
async fn a_value_is_written_out_read_from_a_variable_or_printed_by_a_command() {
    let set = |name: &str| (name == "TOKEN").then(|| "from-env".into());
    for (text, expected) in [
        ("value = 'as-written'", "as-written"),
        ("value = { env = 'TOKEN' }", "from-env"),
        ("value = { cmd = ['printf', 'from cmd\\n'] }", "from cmd"),
    ] {
        let value = parse(text).unwrap();
        value.validate("value").unwrap();
        assert_eq!(value.resolve(set).await.unwrap(), expected, "{text}");
    }
}

#[tokio::test]
async fn a_value_that_cannot_be_read_fails() {
    for (text, message) in [
        ("value = { env = 'UNSET' }", "variable UNSET is not set"),
        ("value = { cmd = ['false'] }", "false exited with"),
        (
            "value = { cmd = ['printf', '\\n'] }",
            "printf printed nothing",
        ),
        (
            "value = { cmd = ['printf', '\\377'] }",
            "printf is not UTF-8",
        ),
    ] {
        let error = parse(text).unwrap().resolve(|_| None).await.unwrap_err();
        assert!(format!("{error:#}").contains(message), "{text}: {error:#}");
    }
}

#[test]
fn each_form_is_validated() {
    for (text, message) in [
        ("value = { env = 'A-B' }", "invalid value variable name"),
        ("value = { env = 'SHOAL_X' }", "conflicts with Shoal"),
        ("value = { cmd = [] }", "value.cmd must name a program"),
        (
            "value = { cmd = [\"a\\u0000b\"] }",
            "value.cmd contains NUL",
        ),
    ] {
        let error = parse(text).unwrap().validate("value").unwrap_err();
        assert!(error.to_string().contains(message), "{text}: {error}");
    }
    for text in [
        "value = { file = 'key' }",
        "value = { env = 'A', cmd = ['b'] }",
        "value = 1",
    ] {
        let error = parse(text).unwrap_err();
        assert!(error.to_string().contains("{ cmd = "), "{text}: {error}");
    }
}

#[test]
fn a_written_value_never_shows_in_what_shoal_prints() {
    let printed = format!("{:?}", parse("value = 'sk-raw'").unwrap());
    assert!(!printed.contains("sk-raw"), "{printed}");
    let printed = format!("{:?}", parse("value = { cmd = ['bw', 'get'] }").unwrap());
    assert!(printed.contains("bw"), "{printed}");
}
