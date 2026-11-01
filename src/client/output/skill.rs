//! What `shoal skill` prints.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// A packaged skill's instructions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub skill: String,
}

/// The skills an install wrote, and those it kept because they were
/// changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub installed: Vec<SkillFile>,
    pub kept: Vec<SkillFile>,
}

/// A skill in a skill directory, which `agents` share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFile {
    pub agents: Vec<String>,
    pub skill: String,
    pub path: PathBuf,
}

outputs!(Skill, Installed);
