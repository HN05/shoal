use super::*;

#[test]
fn a_priority_is_a_name_or_a_number_up_to_a_hundred() {
    let parsed: Vec<Priority> = ["low", "urgent", "0", "60", "100"]
        .into_iter()
        .map(|text| text.parse().unwrap())
        .collect();
    let values = [0, 60, 100].map(|value| Priority::try_from(value).unwrap());
    assert_eq!(
        parsed,
        [&[Priority::LOW, Priority::URGENT][..], &values].concat()
    );
    for refused in ["101", "-1", "high", ""] {
        assert!(refused.parse::<Priority>().is_err(), "{refused}");
    }
}

#[test]
fn a_priority_shows_its_name_where_it_has_one() {
    let shown: Vec<_> = [25, 60]
        .map(|value| Priority::try_from(value).unwrap().to_string())
        .into();
    assert_eq!(shown, ["low", "60"]);
}

#[test]
fn a_priority_is_a_json_number_within_its_range() {
    assert_eq!(serde_json::to_string(&Priority::URGENT).unwrap(), "75");
    assert_eq!(
        serde_json::from_str::<Priority>("30").unwrap(),
        Priority::try_from(30).unwrap()
    );
    assert!(serde_json::from_str::<Priority>("101").is_err());
}
