//! The scope token this process's requests carry: one an approval granted
//! it, else one a starter handed it, else the [`SCOPE_TOKEN`] it inherited.
use super::SCOPE_TOKEN;
use crate::scope_tokens::ScopeToken;

/// The scope token a starter handed this process with a chat to start, or
/// the chat's own once the process runs it, which stands in for
/// [`SCOPE_TOKEN`] without entering any environment.
static HANDED_SCOPE: std::sync::RwLock<Option<ScopeToken>> = std::sync::RwLock::new(None);

/// The scope token this process holds in place of the one it inherited.
fn held_scope() -> Option<ScopeToken> {
    (HANDED_SCOPE.read())
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Hold `token` in place of the scope token this process inherited, as a
/// process that acts for another space from then on, which no grant of the
/// command so far covers.
fn hold_scope(token: ScopeToken) {
    release_grant();
    *(HANDED_SCOPE.write()).unwrap_or_else(std::sync::PoisonError::into_inner) = Some(token);
}

/// The scope token an approval of what this process does granted it, which
/// stands in for its own scope until it releases it.
static GRANTED_SCOPE: std::sync::RwLock<Option<ScopeToken>> = std::sync::RwLock::new(None);

fn granted_scope() -> Option<ScopeToken> {
    (GRANTED_SCOPE.read())
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Act from now on in the scope of `token`, which an approval of what this
/// process does granted it.
pub fn act_under_grant(token: ScopeToken) {
    *(GRANTED_SCOPE.write()).unwrap_or_else(std::sync::PoisonError::into_inner) = Some(token);
}

/// Act in this process's own scope again, as once the daemon that granted
/// a scope token, which knows it alone, exits.
pub fn release_grant() {
    *(GRANTED_SCOPE.write()).unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Act from now on in the scope of `token`, that of the space whose agent
/// this scoped process runs, which outlasts the scope it inherited.
pub fn act_in_space_scope(token: ScopeToken) {
    hold_scope(token);
}

/// The scope token an approval granted this process, else the one it
/// holds, else the one it inherited.
pub fn scope_token() -> Option<ScopeToken> {
    (granted_scope().or_else(held_scope))
        .or_else(|| std::env::var(SCOPE_TOKEN).ok().map(ScopeToken::from))
}

/// True when this process carries cooperative workspace scope.
pub fn is_scoped() -> bool {
    held_scope().is_some() || std::env::var_os(SCOPE_TOKEN).is_some()
}

#[cfg(test)]
mod tests;
