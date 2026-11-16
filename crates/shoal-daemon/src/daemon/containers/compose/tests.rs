use super::*;

#[test]
fn services_list_each_published_port_once_from_either_format() {
    let line = r#"{"Service":"db","State":"running","Publishers":[{"URL":"0.0.0.0","TargetPort":5432,"PublishedPort":49200,"Protocol":"tcp"},{"URL":"::","TargetPort":5432,"PublishedPort":49200,"Protocol":"tcp"},{"URL":"","TargetPort":5433,"PublishedPort":0,"Protocol":"tcp"}]}"#;
    let cache = r#"{"Service":"cache","State":"exited","Publishers":null}"#;
    let expected = vec![
        ContainerService {
            service: "db".into(),
            state: "running".into(),
            ports: vec![PublishedPort {
                target: 5432,
                published: 49200,
                protocol: "tcp".into(),
            }],
        },
        ContainerService {
            service: "cache".into(),
            state: "exited".into(),
            ports: Vec::new(),
        },
    ];
    let lines = format!("{line}\n{}\n", cache.replace("null", "[]"));
    assert_eq!(parse_services(&lines).unwrap(), expected);
    let array = format!("[{line},{}]", cache.replace("null", "[]"));
    assert_eq!(parse_services(&array).unwrap(), expected);
    assert_eq!(parse_services("").unwrap(), []);
}

#[test]
fn a_project_name_is_the_workspace_id_in_compose_spelling() {
    let id = SpaceId::from("3F2a-9C.x");
    assert_eq!(project_name(&id), "shoal-3f2a-9cx");
}
