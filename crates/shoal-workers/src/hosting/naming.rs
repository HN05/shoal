//! What the places hosts open for spaces are called. A place takes a
//! space's new label only while it still shows the one Shoal gave it, since
//! another is the user's.
use tokio::sync::OnceCell;

use crate::places::naming::place_name;
use crate::{
    client::connection::CommandPaths,
    config::Effective,
    model::{Space, Workspace},
    sessions::{HostName, Origin},
    terminal::{Configured, Options, PlaceName, Places},
};

/// What a space's places on one host show: `new` from now on, in place of
/// `expected`, the label Shoal gave them before.
#[derive(Clone)]
pub struct Labels {
    pub new: String,
    pub expected: String,
}

/// The labels of the places hosting `space` after its name changed from
/// `previous`, from one lookup of a workspace's repository and issue, made
/// when a host first needs it.
pub struct Renamed<'a> {
    paths: &'a CommandPaths,
    space: &'a Space,
    previous: &'a str,
    name: OnceCell<Option<PlaceName>>,
}

impl<'a> Renamed<'a> {
    pub fn new(paths: &'a CommandPaths, space: &'a Space, previous: &'a str) -> Self {
        Self {
            paths,
            space,
            previous,
            name: OnceCell::new(),
        }
    }

    /// The labels of the space's places on a host with `options`, rendered
    /// for its name and its previous one. Without a template, or a worktree
    /// to fill one in, the labels are the names; without the lookup a
    /// template's places keep their labels.
    pub async fn labels(&self, options: &Options) -> Labels {
        let (Some(workspace), Some(template)) = (self.space.to_workspace(), &options.name_template)
        else {
            return Labels {
                new: self.space.name.to_string(),
                expected: self.previous.to_owned(),
            };
        };
        let branch = workspace.branch_or_name();
        let Some(name) = self.name(&workspace).await else {
            return Labels {
                new: branch.to_owned(),
                expected: branch.to_owned(),
            };
        };
        let name = name.templated(Some(template.clone()));
        Labels {
            new: name.render(branch, &workspace.name),
            expected: name.render(branch, self.previous),
        }
    }

    /// The place name of `workspace`, the space's, looked up once; a failed
    /// lookup warns.
    async fn name(&self, workspace: &Workspace) -> Option<&PlaceName> {
        let looked_up = self.name.get_or_init(async || {
            place_name(self.paths, workspace)
                .await
                .inspect_err(|error| {
                    eprintln!(
                        "warning: cannot name the places of {}: {error:#}",
                        workspace.name
                    );
                })
                .ok()
        });
        looked_up.await.as_ref()
    }
}

/// Show a space's new label on `places`, the places Shoal created for it,
/// through the hosts `settings` enable that show names, on the server each
/// place records. `labels` gives each such host's labels; a place takes the
/// new one only while it still shows the expected one, and a host whose
/// labels match is skipped. A place that fails warns, naming its host.
pub async fn relabel_space(
    settings: &Effective,
    places: &[Origin],
    labels: impl AsyncFn(&Options) -> Labels,
) {
    for configured in Configured::all(settings).filter(|host| host.options.enabled) {
        let Some(host_places) = configured.host.places().filter(|p| p.names_places()) else {
            continue;
        };
        let name = configured.host.name();
        let hosted: Vec<&Origin> = places.iter().filter(|place| place.host == name).collect();
        if hosted.is_empty() {
            continue;
        }
        let labels = labels(&configured.options).await;
        if labels.new != labels.expected {
            relabel_each(name, host_places, &hosted, &labels).await;
        }
    }
}

/// Show `labels`' new label on each of `hosted`, places of `host`, while
/// it shows the expected one.
async fn relabel_each(host: HostName, places: &dyn Places, hosted: &[&Origin], labels: &Labels) {
    for place in hosted {
        let renamed = places
            .rename_place(place, Some(&labels.expected), &labels.new)
            .await;
        if let Err(error) = renamed {
            eprintln!(
                "warning: cannot relabel the {host} place {}: {error:#}",
                place.id()
            );
        }
    }
}
