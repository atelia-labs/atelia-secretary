use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod domain;
pub mod store;

pub use domain::*;
pub use store::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectId(Uuid);

impl ProjectId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ProjectId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PolicyState {
    Allowed,
    RequiresAudit,
    RequiresHumanApproval,
    Blocked,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AxFeedback {
    pub id: Uuid,
    pub summary: String,
    pub observed: String,
    pub expected: Option<String>,
    pub severity: AxSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AxSeverity {
    Blocker,
    Painful,
    Confusing,
    Minor,
}
