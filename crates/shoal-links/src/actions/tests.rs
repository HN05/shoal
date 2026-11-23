use shoal_daemon_base::callers::Access;
use shoal_daemon_base::callers::Caller;
use shoal_forge_types::items::{
    action::Action,
    review::{NewReview, ReviewVerdict},
};

use super::*;

#[test]
fn a_scoped_verdict_is_the_space_s_own_post_and_the_user_s_is_not() {
    let approval = Action::Review(NewReview {
        verdict: ReviewVerdict::Approve,
        body: "Looks right".into(),
        comments: Vec::new(),
        commit: None,
    });
    let posted = || vec!["review-1".to_owned()];
    let scoped = Caller {
        space_id: "space".into(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    assert_eq!(own_posts(&approval, Some(&scoped), posted()), posted());
    assert!(own_posts(&approval, None, posted()).is_empty());
}
