use serde::Deserialize;

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
        validate(&value, "value").unwrap();
        assert_eq!(resolve(&value, set).await.unwrap(), expected, "{text}");
    }
}

#[tokio::test]
async fn a_value_that_cannot_be_read_fails() {
    for text in [
        "value = { env = 'UNSET' }",
        "value = { cmd = ['false'] }",
        "value = { cmd = ['printf', '\\n'] }",
        "value = { cmd = ['printf', '\\377'] }",
    ] {
        assert!(
            resolve(&parse(text).unwrap(), |_| None).await.is_err(),
            "{text}"
        );
    }
}

#[test]
fn each_form_is_validated() {
    for text in [
        "value = { env = 'A-B' }",
        "value = { env = 'SHOAL_X' }",
        "value = { cmd = [] }",
        "value = { cmd = [\"a\\u0000b\"] }",
    ] {
        assert!(validate(&parse(text).unwrap(), "value").is_err(), "{text}");
    }
    for text in [
        "value = { file = 'key' }",
        "value = { env = 'A', cmd = ['b'] }",
        "value = 1",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}
