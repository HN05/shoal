use clap::ValueEnum;

use super::*;

#[test]
fn value_names_spell_the_labels() {
    for status in ListStatus::value_variants() {
        let name = status.to_possible_value().unwrap();
        assert_eq!(
            name.get_name(),
            status
                .label()
                .replace(" (", " ")
                .replace(')', "")
                .replace(' ', "-")
        );
    }
}
