use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod domain;
pub mod policy;
pub mod runtime;
pub mod settings;
pub mod store;
pub mod tool_output;
pub mod tools;

pub use domain::*;
pub use policy::*;
pub use runtime::*;
pub use settings::*;
pub use store::*;
pub use tool_output::*;
pub use tools::*;

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
