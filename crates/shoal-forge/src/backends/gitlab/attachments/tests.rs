use super::*;

#[test]
fn upload_urls_map_to_the_api_route_for_the_same_file() {
    for (url, route) in [
        (
            "https://gitlab.com/-/project/42/uploads/0123abcd/shot.png",
            "projects/42/uploads/0123abcd/shot.png",
        ),
        (
            "https://gitlab.com/group/sub/project/uploads/0123abcd/screen%20shot.png?inline=false",
            "projects/group%2Fsub%2Fproject/uploads/0123abcd/screen%20shot.png",
        ),
        (
            "https://gitlab.example.com/team/project/uploads/0123abcd/log.txt#top",
            "projects/team%2Fproject/uploads/0123abcd/log.txt",
        ),
        (
            "https://gitlab.com/-/group/7/uploads/0123abcd/shot.png",
            "groups/7/uploads/0123abcd/shot.png",
        ),
        (
            "https://gitlab.com/groups/team/sub/-/uploads/0123abcd/shot.png",
            "groups/team%2Fsub/uploads/0123abcd/shot.png",
        ),
    ] {
        assert_eq!(api_route(url).unwrap(), route, "{url}");
    }
}

#[test]
fn other_urls_are_not_uploads() {
    for url in [
        "https://gitlab.com/team/project/-/issues/3",
        "https://gitlab.com/-/project/abc/uploads/0123abcd/shot.png",
        "https://gitlab.com/-/project//uploads/0123abcd/shot.png",
        "https://gitlab.com/groups/-/uploads/0123abcd/shot.png",
        "https://gitlab.com/project/uploads/0123abcd/shot.png",
        "https://gitlab.com/team/project/uploads/0123abcd/..",
        "https://gitlab.com/team/../uploads/0123abcd/shot.png",
        "https://gitlab.com/team/project/uploads/../shot.png",
        "https://gitlab.com/team/project/uploads/0123abcd/shot.png/more",
    ] {
        assert!(api_route(url).is_err(), "{url}");
    }
}
