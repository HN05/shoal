use super::*;

#[test]
fn findings_are_review_and_number() {
    let id: FindingId = "17.2".parse().unwrap();
    assert_eq!(
        id,
        FindingId {
            review: 17,
            number: 2
        }
    );
    assert_eq!(id.to_string(), "17.2");
    assert_eq!(serde_json::to_string(&id).unwrap(), r#""17.2""#);
    for text in ["17", "17.", ".2", "0.1", "17.0", "a.b", "17.2.1"] {
        assert!(text.parse::<FindingId>().is_err(), "{text}");
    }
}

#[test]
fn every_outcome_carries_exactly_its_data() {
    let reason = || Some("the caller checks it".to_owned());
    let of = Some(FindingId {
        review: 3,
        number: 1,
    });
    assert_eq!(
        Outcome::new(OutcomeKind::Fixed, None, None, None).unwrap(),
        Outcome::Fixed
    );
    assert!(matches!(
        Outcome::new(OutcomeKind::FalsePositive, reason(), None, None).unwrap(),
        Outcome::FalsePositive { .. }
    ));
    assert!(matches!(
        Outcome::new(OutcomeKind::WontFix, reason(), None, None).unwrap(),
        Outcome::WontFix { .. }
    ));
    assert!(matches!(
        Outcome::new(OutcomeKind::Deferred, None, Some("#12".into()), None).unwrap(),
        Outcome::Deferred { .. }
    ));
    assert!(matches!(
        Outcome::new(OutcomeKind::Duplicate, None, None, of).unwrap(),
        Outcome::Duplicate { .. }
    ));
    for (kind, reason, issue, of) in [
        (OutcomeKind::Fixed, reason(), None, None),
        (OutcomeKind::FalsePositive, None, None, None),
        (OutcomeKind::FalsePositive, Some("  ".into()), None, None),
        (OutcomeKind::WontFix, reason(), Some("#1".into()), None),
        (OutcomeKind::Deferred, None, None, None),
        (OutcomeKind::Duplicate, reason(), None, None),
    ] {
        assert!(Outcome::new(kind, reason, issue, of).is_err(), "{kind}");
    }
}

#[test]
fn outcomes_are_tagged_with_their_data() {
    let outcome = Outcome::Duplicate {
        of: FindingId {
            review: 3,
            number: 1,
        },
    };
    let json = serde_json::to_string(&outcome).unwrap();
    assert_eq!(json, r#"{"outcome":"duplicate","of":"3.1"}"#);
    assert_eq!(serde_json::from_str::<Outcome>(&json).unwrap(), outcome);
    for missing in [
        r#"{"outcome":"false-positive"}"#,
        r#"{"outcome":"deferred"}"#,
        r#"{"outcome":"duplicate","of":"3"}"#,
    ] {
        assert!(
            serde_json::from_str::<Outcome>(missing).is_err(),
            "{missing}"
        );
    }
}

#[test]
fn status_counts_findings_by_severity() {
    let finding = |number, severity| Finding {
        id: FindingId { review: 1, number },
        severity,
        path: "src/a.rs".into(),
        line: 1,
        title: "t".into(),
        body: "b".into(),
        replies: Vec::new(),
        outcome: None,
        check: None,
    };
    let findings = [
        finding(1, Severity::ShouldFix),
        finding(2, Severity::Blocker),
        finding(3, Severity::ShouldFix),
    ];
    assert_eq!(severity_counts(&findings), "1 blocker, 2 should-fix");
    assert_eq!(severity_counts(&[]), "");
}
