use super::judged;
use crate::daemon::target_checks::Facts;
use crate::{model::Space, state::WorkspaceState};
use shoal_protocol::{
    model::Offer,
    protocol::{AssignmentMethod, Operation, SpaceMethod, WorkspaceMethod},
    space_names::SpaceSelector,
};

/// A workspace in `state`, numbered 4.
fn workspace(state: WorkspaceState) -> Space {
    let path = std::path::Path::new("/work");
    let mut workspace = shoal_protocol::test_support::workspace("w1", "r1", "fix", path);
    workspace.part.state = state;
    workspace.number = 4.into();
    Space::from(workspace)
}

/// The one offer `acting` makes of `space`.
fn offer(acting: Operation, space: Space) -> Offer {
    let [judged] = <[_; 1]>::try_from(judged(&acting, &[space], &Facts::default())).unwrap();
    judged.offer
}

/// A placeholder for the space a picker is about to pick.
fn picking() -> SpaceSelector {
    SpaceSelector::Number(shoal_ids::space_ids::SpaceNumber::new(0))
}

#[test]
fn a_request_for_a_ready_workspace_refuses_a_preparing_one_with_its_reason() {
    let refresh = || {
        Operation::Workspace(WorkspaceMethod::RefreshWorkspace {
            workspace: picking(),
        })
    };
    assert_eq!(
        offer(refresh(), workspace(WorkspaceState::Ready)),
        Offer::Accepted
    );
    let Offer::Refused { reason } = offer(refresh(), workspace(WorkspaceState::Preparing)) else {
        panic!("a preparing workspace is not refused");
    };
    assert!(reason.contains("shoal status 4"), "{reason}");
}

#[test]
fn a_space_without_what_the_request_acts_on_is_left_out() {
    let undo = Operation::Assignment(AssignmentMethod::UndoDone { space: picking() });
    assert_eq!(
        offer(undo, workspace(WorkspaceState::Ready)),
        Offer::Lacking
    );
}

#[test]
fn a_request_that_names_no_space_takes_any() {
    let listing = Operation::Space(SpaceMethod::ListSpaces);
    assert_eq!(
        offer(listing, workspace(WorkspaceState::Failed)),
        Offer::Accepted
    );
}
