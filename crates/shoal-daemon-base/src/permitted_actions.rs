//! An action a permission key governs, as a scoped caller asks to take it,
//! which the daemon lets it take as its space's permission for the key
//! allows.
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::config::permissions::PermissionKey;

/// An action a permission governs, as a scoped caller asks to take it.
#[derive(Clone)]
pub struct Action {
    pub key: PermissionKey,
    /// What it does, such as a command with its arguments, which approval
    /// binds: another action needs another decision.
    pub action: String,
    /// The registered repository it acts in, whose config resolves the
    /// permission of a chat without one; none for an action outside any.
    pub repository: Option<RepositoryId>,
}

impl Action {
    /// `action`, which `key` governs, outside any repository.
    pub fn new(key: PermissionKey, action: impl Into<String>) -> Self {
        Self {
            key,
            action: action.into(),
            repository: None,
        }
    }

    /// This action, taken in the registered repository `repository`.
    pub fn in_repository(self, repository: RepositoryId) -> Self {
        Self {
            repository: Some(repository),
            ..self
        }
    }

    /// Sending `request`, which `key` governs, in `repository`, named as
    /// [`request_text`] names it.
    pub fn request(
        key: PermissionKey,
        request: &impl serde::Serialize,
        repository: Option<RepositoryId>,
    ) -> Self {
        Self {
            key,
            action: request_text(request),
            repository,
        }
    }
}

/// The longest request an action names whole, and how much of a longer
/// one it shows.
const NAMED_WHOLE: usize = 160;

/// `request` as an action names it: its JSON, or for a long one the start
/// of it, in the order its fields are written, such as a forge API
/// request's method and path, and a digest of all of it, so that another
/// request is another action.
fn request_text(request: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    let text = serde_json::to_string(request).unwrap_or_default();
    if text.len() <= NAMED_WHOLE {
        return text;
    }
    let shown = (text.char_indices())
        .take_while(|(at, _)| *at < NAMED_WHOLE)
        .map(|(_, c)| c)
        .collect::<String>();
    let digest: String = (Sha256::digest(text.as_bytes()).iter().take(8))
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{shown}… (sha256 {digest})")
}

#[cfg(test)]
mod tests;
