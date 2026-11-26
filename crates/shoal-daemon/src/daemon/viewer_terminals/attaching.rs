//! Attaching a viewer on another machine, or a device, to an agent's
//! session here. The attach runs the same code a local `shoal attach`
//! does, on a terminal the daemon makes for the viewer.
use anyhow::Result;
use shoal_protocol::{attaching::Viewer, space_names::SpaceSelector};

use super::{REPORT, Reports, ViewerTerminal};
use crate::{
    daemon::manager::Manager,
    workers::{Worker, internal_command},
};

impl Manager {
    /// Start Shoal's attach to a session of the space `selector` names on a
    /// terminal made for `viewer`.
    pub(crate) async fn open_viewer_terminal(
        &self,
        selector: &SpaceSelector,
        viewer: &Viewer,
    ) -> Result<ViewerTerminal> {
        let space = self.spaces.space(selector).await?;
        let machine = self.hub.machine_name(self).await;
        let viewer_json = serde_json::to_string(viewer)?;
        let argv = internal_command(
            &self.paths,
            false,
            Worker::Attach {
                space: &space.id,
                viewer: &viewer_json,
                machine: &machine,
                report: REPORT,
            },
        )?;
        ViewerTerminal::start(&argv, &viewer.terminal, &self.paths.home, Reports::Yes)
    }
}
