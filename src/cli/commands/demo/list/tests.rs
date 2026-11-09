use super::*;
use crate::demos::{Demo, Launch, Running};
use crate::time::UnixTime;

fn demo(cmd: &[&str]) -> Demo {
    Demo {
        cmd: cmd.iter().map(|arg| (*arg).to_owned()).collect(),
        preset: None,
        open: None,
        scheme: None,
        ports: None,
    }
}

#[test]
fn listings_show_running_demos_then_the_configured_ones_not_running() {
    let launch = Launch {
        id: "launch".into(),
        name: "web".into(),
        preset: Some(Preset::Web),
        open: None,
        scheme: None,
        environment: BTreeMap::new(),
        origin: crate::sessions::Origin::created(
            crate::sessions::HostName::Zmx,
            "first-web".into(),
        ),
    };
    let listing = Listing {
        workspace: Some("first".into()),
        configured: BTreeMap::from([
            ("tui".to_owned(), demo(&["cargo", "run"])),
            ("web".to_owned(), demo(&["npm", "run", "dev"])),
        ]),
        running: vec![Running {
            workspace_id: "id".into(),
            workspace: "first".into(),
            execution_id: "execution".into(),
            started_at: UnixTime::from_seconds(0),
            has_run: true,
            url: None,
            launch,
        }],
    };
    let rows = listing_rows(&listing, Palette::stdout(true));
    let names: Vec<_> = rows
        .iter()
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    assert_eq!(names, ["web", "tui"]);
}
