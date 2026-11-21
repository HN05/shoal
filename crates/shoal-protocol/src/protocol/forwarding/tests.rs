use super::*;
use crate::protocol::{AlertMethod, ThreadMethod};
use crate::threads::ThreadKind;

#[test]
fn a_withdrawal_targets_a_space_only_when_it_names_the_recipient() {
    let withdraw = |space: Option<SpaceSelector>| {
        Operation::Alert(AlertMethod::WithdrawAlert {
            id: 7,
            space,
            from: None,
        })
    };
    assert_eq!(withdraw(None).target(), None);
    let space = SpaceSelector::Number(3.into());
    let named = withdraw(Some(space.clone()));
    assert_eq!(named.target(), Some(Target::Space(&space)));
}

#[test]
fn a_thread_opened_as_a_space_targets_none_and_one_opened_with_a_space_acts_there() {
    let open = |from: Option<SpaceSelector>, to: Option<SpaceSelector>| {
        Operation::Thread(ThreadMethod::OpenThread {
            name: "Which base?".to_owned(),
            kind: ThreadKind::Question,
            to,
            from,
            code: Vec::new(),
        })
    };
    let to = SpaceSelector::Number(4.into());
    let from = SpaceSelector::Number(2.into());
    let with = open(None, Some(to.clone()));
    assert_eq!(with.target(), Some(Target::Acting(&to)));
    assert_eq!(open(Some(from), Some(to)).target(), None);
}

#[test]
fn a_thread_named_by_its_own_number_alone_targets_none() {
    let show = |thread| Operation::Thread(ThreadMethod::ShowThread { thread });
    let full = ItemNumber {
        space: 5.into(),
        item: 2,
    };
    let shown = show(ThreadNumber::Full(full));
    assert_eq!(shown.target(), Some(Target::Item(&full)));
    assert_eq!(show(ThreadNumber::Own(2)).target(), None);
}

#[test]
fn a_forge_request_targets_the_space_of_its_context_and_none_of_a_checkout() {
    let space = SpaceSelector::Number(6.into());
    let labels = |context| {
        Operation::Item(crate::protocol::ItemMethod::Labels {
            target: RepoTarget {
                context: Some(context),
                repository: None,
            },
        })
    };
    let in_space = labels(ItemContext::space(space.clone()));
    assert_eq!(in_space.target(), Some(Target::Space(&space)));
    let checkout = labels(ItemContext::Checkout(
        shoal_ids::repository_ids::RepositoryId::generate(),
    ));
    assert_eq!(checkout.target(), None);
}
