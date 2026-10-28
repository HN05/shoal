use super::*;

#[test]
fn places_read_as_ordinals() {
    let read: Vec<_> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 103, 111]
        .into_iter()
        .map(ordinal)
        .collect();
    assert_eq!(
        read,
        [
            "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "103rd", "111th"
        ]
    );
}
