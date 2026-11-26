//! The checks an answer must pass: what each component that enters a value
//! it takes accepts, and the `@checks` of those components and of its
//! button, read against the data the answer enters, as a renderer holds an
//! input to its bounds and disables a button or flags an input whose check
//! fails.
use anyhow::{Result, bail};
use serde_json::Value;

use super::{Component, Placed, Surface, SurfaceAction, functions::truth};

impl Surface {
    /// Refuse `action` where a value it takes, given or kept, is one a
    /// component that enters it does not accept, such as a date before its
    /// earliest, or where a check of its button or of such a component
    /// fails, naming the check's message. A check Shoal cannot work out,
    /// one only a renderer runs, passes.
    pub(super) fn check_answer(&self, action: &SurfaceAction) -> Result<()> {
        self.check_values(action)?;
        let button = (self.components())
            .find(|component| component.event_name() == Some(action.event.as_str()))
            .map(|button| (button, action.scope.as_str()));
        let inputs = (action.inputs.iter())
            .flat_map(|input| &input.controls)
            .filter_map(|(id, scope)| Some((self.component(id)?, scope.as_str())));
        for (component, scope) in button.into_iter().chain(inputs) {
            if let Some(failed) = self.failed_check(component, scope) {
                bail!("{} refuses the answer: {failed}", component.id);
            }
        }
        Ok(())
    }

    /// Refuse a value of `action`, as the entered data holds it, that a
    /// component entering it does not accept with the bounds that data
    /// sets.
    fn check_values(&self, action: &SurfaceAction) -> Result<()> {
        for input in &action.inputs {
            let Some(value) = self.data(&input.path).filter(|value| !value.is_null()) else {
                continue;
            };
            for (id, scope) in &input.controls {
                let Some(component) = self.component(id) else {
                    continue;
                };
                let placed = Placed {
                    component,
                    scope: scope.clone(),
                    items: Vec::new(),
                };
                if let Some(kind) = self.entry(&placed) {
                    kind.accept(&input.key, value.clone())?;
                }
            }
        }
        Ok(())
    }

    /// The message of the first of `component`'s checks that fails where
    /// it is placed in `scope`.
    fn failed_check(&self, component: Component<'_>, scope: &str) -> Option<String> {
        let rules = component.get("@checks")?.as_array()?;
        rules.iter().find_map(|rule| {
            let passes = truth(&self.known(rule.get("condition")?, scope)?);
            let message = rule.get("message").and_then(Value::as_str);
            (!passes).then(|| message.unwrap_or("a check fails").to_owned())
        })
    }
}
