use super::Span;

#[test]
fn reads_units_largest_first() {
    let seconds = |text: &str| text.parse::<Span>().map(|span| span.duration().as_secs());
    assert_eq!(seconds("90s").unwrap(), 90);
    assert_eq!(seconds("1h30m").unwrap(), 5400);
    assert_eq!(seconds("2d").unwrap(), 172_800);
    assert_eq!(seconds("1w").unwrap(), 604_800);
    for refused in ["", "10", "m", "30m1h", "1h1h", "0s", "1y", "-1m"] {
        assert!(seconds(refused).is_err(), "{refused:?} should be refused");
    }
}

#[test]
fn writes_what_it_reads() {
    for text in ["90s", "1h30m", "1d", "1w2d3h4m5s"] {
        let span: Span = text.parse().unwrap();
        let written = span.to_string();
        assert_eq!(
            written.parse::<Span>().unwrap(),
            span,
            "{text} wrote {written}"
        );
    }
    assert_eq!("90s".parse::<Span>().unwrap().to_string(), "1m30s");
}
