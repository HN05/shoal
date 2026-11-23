use super::*;

/// Another machine that prints the next of `answers` for each command.
struct Machine {
    answers: Mutex<Vec<Result<String, Unanswered>>>,
}

impl Remote for Machine {
    fn run<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a [String],
    ) -> BoxFuture<'a, Result<String, Unanswered>> {
        let answer = self.answers.lock().unwrap().remove(0);
        Box::pin(async move { answer })
    }
}

fn values(answers: Vec<Result<String, Unanswered>>) -> Values {
    let values = Values::default();
    let answers = Mutex::new(answers);
    values.connect(Arc::new(Machine { answers }));
    values
}

fn on_desk() -> Value {
    Value::Cmd {
        cmd: vec!["security".into()],
        on: Some("desk".into()),
    }
}

fn unreachable() -> Result<String, Unanswered> {
    Err(Unanswered::Unreachable(anyhow!("desk is offline")))
}

const SETTING: &str = "machine.forges.\"git.example.com\".agent_token";

#[tokio::test]
async fn the_last_value_answers_while_its_machine_cannot_be_reached() {
    let values = values(vec![
        Ok("first".into()),
        unreachable(),
        Ok("rotated".into()),
    ]);
    assert_eq!(values.resolve(SETTING, &on_desk()).await.unwrap(), "first");
    assert_eq!(values.resolve(SETTING, &on_desk()).await.unwrap(), "first");
    assert_eq!(
        values.resolve(SETTING, &on_desk()).await.unwrap(),
        "rotated"
    );
}

#[tokio::test]
async fn without_a_kept_value_an_unreachable_machine_fails_naming_it_and_the_setting() {
    let values = values(vec![unreachable()]);
    let error = values.resolve(SETTING, &on_desk()).await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("desk") && error.contains(SETTING), "{error}");
}

#[tokio::test]
async fn a_failed_command_never_falls_back_on_the_kept_value() {
    let failed = Err(Unanswered::Failed(anyhow!("security exited with 1")));
    let values = values(vec![Ok("first".into()), failed]);
    values.resolve(SETTING, &on_desk()).await.unwrap();
    assert!(values.resolve(SETTING, &on_desk()).await.is_err());
}

#[tokio::test]
async fn a_forgotten_value_no_longer_answers() {
    for forget in [
        (|values: &Values| values.forget(&on_desk())) as fn(&Values),
        |values| values.forget_all(),
    ] {
        let values = values(vec![Ok("first".into()), unreachable()]);
        values.resolve(SETTING, &on_desk()).await.unwrap();
        forget(&values);
        assert!(values.resolve(SETTING, &on_desk()).await.is_err());
    }
}

#[tokio::test]
async fn a_machine_joined_to_no_hub_reads_no_other_machines_value() {
    let error = Values::default().resolve(SETTING, &on_desk()).await;
    assert!(format!("{:#}", error.unwrap_err()).contains("joined to no hub"));
}
