//! The checks an answer must pass: the `@checks` of its button and of the
//! inputs it takes, read against the data the answer enters, as a renderer
//! disables a button or flags an input whose check fails.
use anyhow::{Result, bail};
use serde_json::Value;

use super::{Component, Surface, SurfaceAction, functions::truth};

impl Surface {
    /// Refuse `action` where a check of its button, or of an input it
    /// takes, fails, naming the check's message. A check Shoal cannot work
    /// out, one only a renderer runs, passes.
    pub(super) fn check_answer(&self, action: &SurfaceAction) -> Result<()> {
        let button = (self.components())
            .find(|component| component.event_name() == Some(action.event.as_str()))
            .map(|button| (button, action.scope.as_str()));
        let inputs = (action.inputs.iter())
            .filter_map(|input| Some((self.component(&input.component)?, input.scope.as_str())));
        for (component, scope) in button.into_iter().chain(inputs) {
            if let Some(failed) = self.failed_check(component, scope) {
                bail!("{} refuses the answer: {failed}", component.id);
            }
        }
        Ok(())
    }

    /// The message of the first of `component`'s checks that fails where
    /// it is placed in `scope`.
    fn failed_check(&self, component: Component<'_>, scope: &str) -> Option<String> {
        let rules = component.get("@checks")?.as_array()?;
        rules.iter().find_map(|rule| {
            let passes = truth(&self.known(rule.get("condition")?, scope)?)?;
            let message = rule.get("message").and_then(Value::as_str);
            (!passes).then(|| message.unwrap_or("a check fails").to_owned())
        })
    }
}
