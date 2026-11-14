//! What the gate returns: the output schema it is given, and the verdict
//! read from its result, whose escalation must quote a case the user wrote.
use serde::Deserialize;
use serde_json::Value;

/// The JSON schema of the gate's result. Every field is required, as Codex's
/// strict schemas ask; `rule` is empty unless the verdict escalates.
pub(crate) const SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["verdict", "reason", "rule"],
  "properties": {
    "verdict": {"type": "string", "enum": ["allow", "deny", "escalate"]},
    "reason": {"type": "string"},
    "rule": {"type": "string"}
  }
}"#;

/// The gate's decision on a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow {
        reason: String,
    },
    Deny {
        reason: String,
    },
    /// Pass it on for `rule`, one of the cases the user wrote down.
    Escalate {
        reason: String,
        rule: String,
    },
}

impl Verdict {
    pub(crate) fn reason(&self) -> &str {
        match self {
            Self::Allow { reason } | Self::Deny { reason } | Self::Escalate { reason, .. } => {
                reason
            }
        }
    }
}

/// Why a result is no verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Invalid {
    /// It does not match the schema, or a denial or escalation gives no
    /// reason.
    Malformed,
    /// An escalation quotes no case of `escalate` word for word.
    Misquoted(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    verdict: String,
    reason: String,
    rule: String,
}

/// The verdict in `value`, an escalation of which must quote one of
/// `escalate` exactly.
pub(crate) fn read(value: Value, escalate: &[String]) -> Result<Verdict, Invalid> {
    let output: Output = serde_json::from_value(value).map_err(|_| Invalid::Malformed)?;
    let reason = output.reason.trim().to_owned();
    let verdict = match output.verdict.as_str() {
        "allow" => Verdict::Allow { reason },
        "deny" if !reason.is_empty() => Verdict::Deny { reason },
        "escalate" if !reason.is_empty() => {
            let rule = output.rule.trim();
            if !escalate.iter().any(|case| case.trim() == rule) {
                return Err(Invalid::Misquoted(output.rule));
            }
            Verdict::Escalate {
                reason,
                rule: rule.to_owned(),
            }
        }
        _ => return Err(Invalid::Malformed),
    };
    Ok(verdict)
}

#[cfg(test)]
mod tests;
