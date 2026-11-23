//! The change to the merge records an execution makes once it ends.
use shoal_protocol::model::{ExecutionPlan, MergePlan, UnmergePlan};

/// The change to the merge records an execution makes once it ends.
pub enum RecordChange {
    Merge(Box<MergePlan>),
    Unmerge(Box<UnmergePlan>),
}

impl RecordChange {
    /// The change `plan`'s execution makes, if it merges or unmerges.
    pub fn of(plan: &ExecutionPlan) -> Option<Self> {
        match (&plan.merge, &plan.unmerge) {
            (Some(merge), _) => Some(Self::Merge(merge.clone())),
            (None, Some(unmerge)) => Some(Self::Unmerge(unmerge.clone())),
            (None, None) => None,
        }
    }
}
