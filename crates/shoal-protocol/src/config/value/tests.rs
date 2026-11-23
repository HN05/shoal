use super::*;

fn parse(text: &str) -> Value {
    #[derive(Deserialize)]
    struct Setting {
        value: Value,
    }
    toml::from_str::<Setting>(text).unwrap().value
}

#[test]
fn a_written_value_never_shows_in_what_shoal_prints() {
    let printed = format!("{:?}", parse("value = 'sk-raw'"));
    assert!(!printed.contains("sk-raw"), "{printed}");
    let printed = format!("{:?}", parse("value = { cmd = ['bw', 'get'] }"));
    assert!(printed.contains("bw"), "{printed}");
}
