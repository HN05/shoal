use super::*;
use crate::space_names::SpaceSelector;

#[test]
fn a_withdrawal_goes_to_another_machine_only_when_it_names_the_recipient() {
    let withdraw = |space: Option<SpaceSelector>| {
        Operation::Alert(AlertMethod::WithdrawAlert {
            id: 7,
            space,
            from: None,
        })
        .forwarding()
    };
    assert_eq!(withdraw(None), Forwarding::Never);
    let named = withdraw(Some(SpaceSelector::Number(3.into())));
    assert_eq!(named, Forwarding::Online);
}
