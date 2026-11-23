use super::*;

#[test]
fn serializes_the_kind_beside_the_agents_space() -> Result<()> {
    let space_id = SpaceId::generate();
    let agent = Acquirer::Agent {
        space_id: space_id.clone(),
    };
    let json = serde_json::to_value(&agent)?;
    assert_eq!(
        json,
        serde_json::json!({"kind": "agent", "space_id": space_id.to_string()})
    );
    assert_eq!(serde_json::from_value::<Acquirer>(json)?, agent);
    assert_eq!(
        serde_json::to_value(Acquirer::User)?,
        serde_json::json!({"kind": "user"})
    );
    Ok(())
}

#[test]
fn rejects_an_unknown_kind() {
    let unknown = serde_json::json!({"kind": "robot", "execution_id": "x"});
    assert!(serde_json::from_value::<Acquirer>(unknown).is_err());
}

#[test]
fn only_the_user_goes_without_an_id() -> Result<()> {
    let execution = ExecutionId::generate();
    assert_eq!(
        Acquirer::from_parts(AcquirerKind::Demo, Some(execution.to_string()))?,
        Acquirer::Demo {
            execution_id: execution.clone()
        }
    );
    let space = SpaceId::generate();
    let agent = Acquirer::from_parts(AcquirerKind::Agent, Some(space.to_string()))?;
    assert_eq!(agent.agent_space(), Some(&space));
    assert!(Acquirer::from_parts(AcquirerKind::User, Some(execution.to_string())).is_err());
    assert!(Acquirer::from_parts(AcquirerKind::Agent, None).is_err());
    Ok(())
}

#[test]
fn only_the_users_acquire_takes_a_lease_over() {
    let agent = Acquirer::Agent {
        space_id: SpaceId::generate(),
    };
    let other = Acquirer::Agent {
        space_id: SpaceId::generate(),
    };
    assert_eq!(agent.renewed_by(&Acquirer::User), Acquirer::User);
    assert_eq!(Acquirer::User.renewed_by(&agent), Acquirer::User);
    assert_eq!(agent.renewed_by(&other), agent);
}
