use super::*;

/// A simulator recorded before acquirers were is the user's.
#[test]
fn simulators_recorded_without_an_acquirer_are_the_users() -> Result<()> {
    let record = serde_json::json!({
        "id": "sim", "udid": "device", "device": "Phone", "runtime": "iOS",
        "workspace_id": "workspace", "last_workspace_id": null, "lease_name": "default",
        "reason": null, "state": "leased", "last_used": 1, "error": null
    });
    let sim: crate::sim::Simulator = serde_json::from_value(record)?;
    assert_eq!(sim.acquirer, Acquirer::User);
    Ok(())
}

#[test]
fn serializes_the_kind_beside_the_execution() -> Result<()> {
    let execution_id = ExecutionId::generate();
    let agent = Acquirer::Agent {
        execution_id: execution_id.clone(),
    };
    let json = serde_json::to_value(&agent)?;
    assert_eq!(
        json,
        serde_json::json!({"kind": "agent", "execution_id": execution_id.to_string()})
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
fn only_the_user_goes_without_an_execution() -> Result<()> {
    let execution = ExecutionId::generate();
    assert_eq!(
        Acquirer::from_parts(AcquirerKind::Demo, Some(execution.clone()))?,
        Acquirer::Demo {
            execution_id: execution.clone()
        }
    );
    assert!(Acquirer::from_parts(AcquirerKind::User, Some(execution)).is_err());
    assert!(Acquirer::from_parts(AcquirerKind::Agent, None).is_err());
    Ok(())
}

#[test]
fn only_the_users_acquire_takes_a_lease_over() {
    let agent = Acquirer::Agent {
        execution_id: ExecutionId::generate(),
    };
    let other = Acquirer::Agent {
        execution_id: ExecutionId::generate(),
    };
    assert_eq!(agent.renewed_by(&Acquirer::User), Acquirer::User);
    assert_eq!(Acquirer::User.renewed_by(&agent), Acquirer::User);
    assert_eq!(agent.renewed_by(&other), agent);
}
