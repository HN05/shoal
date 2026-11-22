//! What else ends with an agent's turn.
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Turns;

use crate::daemon::manager::Manager;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

impl Turns for Manager {
    fn end_marked_turn<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        execution: Option<&'a ExecutionId>,
        final_message: Option<String>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.end_marked_turn(selector, execution, final_message))
    }
}
