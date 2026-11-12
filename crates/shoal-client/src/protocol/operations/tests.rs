use super::*;

fn issue(number: u64) -> LaunchIssue {
    LaunchIssue {
        url: format!("https://forge.example/team/app/issues/{number}")
            .parse()
            .unwrap(),
        title: format!("Issue {number}"),
        labels: Vec::new(),
    }
}

#[test]
fn a_launch_takes_the_default_issue_only_without_a_task_of_its_own() {
    let mut bare = LaunchRequest::new(None);
    bare.default_issue(issue(1));
    assert_eq!(bare.issue.map(|issue| issue.title), Some("Issue 1".into()));
    let told = [
        LaunchRequest {
            issue: Some(issue(2)),
            ..LaunchRequest::new(None)
        },
        LaunchRequest {
            prompt: Some("Fix the build".into()),
            ..LaunchRequest::new(None)
        },
        LaunchRequest {
            args: vec!["--resume".into()],
            ..LaunchRequest::new(None)
        },
    ];
    for mut launch in told {
        let before = launch.issue.as_ref().map(|issue| issue.title.clone());
        launch.default_issue(issue(1));
        assert_eq!(launch.issue.map(|issue| issue.title), before);
    }
}
