use super::{Action, chat_action};
use crate::cli::{Command, args::lifecycle::LifecycleCommand};
use crate::client::space_names::SpaceSelector;

/// The stop key on a chat row stops the chat and the spaces below it.
#[test]
fn the_stop_key_stops_a_chat() {
    let chat = SpaceSelector::parse("talk");

    let command = chat_action(Action::Stop, chat.clone()).unwrap();

    let Command::Lifecycle(LifecycleCommand::Stop {
        targets,
        skip_children: false,
        interrupt: false,
        processes: false,
        leftovers: false,
        ..
    }) = command
    else {
        panic!("not a stop");
    };
    assert_eq!(targets.space, [chat]);
    assert!(!targets.all);
}
