use clap::ValueEnum;

states!(
    /// Test spellings that clap cannot infer from the Rust names.
    #[derive(Default)]
    Example: ValueEnum {
        First => "first_value",
        #[default]
        Second => "other",
    }
);

#[test]
fn optional_default_and_clap_values_share_wire_spellings() {
    assert_eq!(Example::default(), Example::Second);
    assert_eq!(
        Example::value_variants(),
        &[Example::First, Example::Second]
    );
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for (value, wire) in [(Example::First, "first_value"), (Example::Second, "other")] {
        assert_eq!(value.to_string(), wire);
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Example>(wire.into()).unwrap(),
            value
        );
        assert_eq!(value.to_possible_value().unwrap().get_name(), wire);
        assert_eq!(Example::from_str(wire, false).unwrap(), value);
        assert_eq!(
            db.query_row("SELECT ?1", [value], |row| row.get::<_, String>(0))
                .unwrap(),
            wire
        );
        assert_eq!(
            db.query_row("SELECT ?1", [wire], |row| row.get::<_, Example>(0))
                .unwrap(),
            value
        );
    }
    for invalid in ["First", "first-value", "second", "unknown"] {
        assert!(Example::from_str(invalid, false).is_err());
        assert!(serde_json::from_value::<Example>(invalid.into()).is_err());
        assert!(
            db.query_row("SELECT ?1", [invalid], |row| row.get::<_, Example>(0))
                .is_err()
        );
    }
}

/// A key whose spelling joins its parts.
#[derive(Debug, PartialEq)]
struct Pair(String, String);

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.0, self.1)
    }
}

impl std::str::FromStr for Pair {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        let (first, second) = value
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("no separator in {value}"))?;
        Ok(Self(first.into(), second.into()))
    }
}

text_key!(Pair);

#[test]
fn text_keys_store_and_parse_their_spelling() {
    let pair = Pair("repo".into(), "pool".into());
    let db = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(
        db.query_row("SELECT ?1", [&pair], |row| row.get::<_, String>(0))
            .unwrap(),
        "repo:pool"
    );
    assert_eq!(
        db.query_row("SELECT 'repo:pool'", [], |row| row.get::<_, Pair>(0))
            .unwrap(),
        pair
    );
    assert_eq!(serde_json::to_value(&pair).unwrap(), "repo:pool");
    assert_eq!(
        serde_json::from_value::<Pair>("repo:pool".into()).unwrap(),
        pair
    );
    assert!(
        db.query_row("SELECT 'repo'", [], |row| row.get::<_, Pair>(0))
            .is_err()
    );
    assert!(serde_json::from_value::<Pair>("repo".into()).is_err());
}
