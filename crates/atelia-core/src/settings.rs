//! Configurable defaults for agent-facing tool output.
//!
//! These types keep rarely changed AX knobs out of individual tool call
//! schemas while still giving the runtime a validated, auditable settings
//! surface.

use crate::domain::{Actor, LedgerTimestamp, RepositoryId};
use crate::tool_output::{OutputFormat, RenderOptions};
use crate::ProjectId;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

pub const TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_INLINE_BYTES: u64 = 16 * 1024;
pub const MIN_MAX_INLINE_BYTES: u64 = 256;
pub const MAX_MAX_INLINE_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_MAX_INLINE_LINES: u32 = 200;
pub const MIN_MAX_INLINE_LINES: u32 = 1;
pub const MAX_MAX_INLINE_LINES: u32 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputSettingsScope {
    pub level: ToolOutputSettingsLevel,
    pub tool_id: Option<String>,
}

impl ToolOutputSettingsScope {
    pub fn workspace() -> Self {
        Self {
            level: ToolOutputSettingsLevel::Workspace,
            tool_id: None,
        }
    }

    pub fn repository(repository_id: RepositoryId) -> Self {
        Self {
            level: ToolOutputSettingsLevel::Repository { repository_id },
            tool_id: None,
        }
    }

    pub fn project(project_id: ProjectId) -> Self {
        Self {
            level: ToolOutputSettingsLevel::Project { project_id },
            tool_id: None,
        }
    }

    pub fn session(session_id: impl Into<String>) -> Self {
        Self {
            level: ToolOutputSettingsLevel::Session {
                session_id: session_id.into(),
            },
            tool_id: None,
        }
    }

    pub fn agent_profile(agent_id: impl Into<String>) -> Self {
        Self {
            level: ToolOutputSettingsLevel::AgentProfile {
                agent_id: agent_id.into(),
            },
            tool_id: None,
        }
    }

    pub fn for_tool(mut self, tool_id: impl Into<String>) -> Self {
        self.tool_id = Some(tool_id.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputSettingsLevel {
    Workspace,
    Repository { repository_id: RepositoryId },
    Project { project_id: ProjectId },
    Session { session_id: String },
    AgentProfile { agent_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputDefaults {
    pub render_options: RenderOptions,
    pub max_inline_bytes: u64,
    pub max_inline_lines: u32,
    pub verbosity: ToolOutputVerbosity,
    pub granularity: ToolOutputGranularity,
    pub oversize_policy: OversizeOutputPolicy,
}

impl ToolOutputDefaults {
    pub fn render_options(&self) -> RenderOptions {
        self.render_options.clone()
    }

    pub fn with_overrides(
        &self,
        overrides: &ToolOutputOverrides,
    ) -> Result<Self, ToolOutputSettingsError> {
        let mut defaults = self.clone();
        overrides.apply_to(&mut defaults);
        defaults.validate()?;
        Ok(defaults)
    }

    pub fn validate(&self) -> Result<(), ToolOutputSettingsError> {
        validate_inline_bytes(self.max_inline_bytes)?;
        validate_inline_lines(self.max_inline_lines)?;
        Ok(())
    }
}

impl Default for ToolOutputDefaults {
    fn default() -> Self {
        Self {
            render_options: RenderOptions::default(),
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: DEFAULT_MAX_INLINE_LINES,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::KeyFields,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputVerbosity {
    Minimal,
    Normal,
    Expanded,
    Debug,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputGranularity {
    Summary,
    KeyFields,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OversizeOutputPolicy {
    TruncateWithMetadata,
    SpillToArtifactRef,
    RejectOversize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputOverrides {
    pub format: Option<OutputFormat>,
    pub include_policy: Option<bool>,
    pub include_diagnostics: Option<bool>,
    pub include_cost: Option<bool>,
    pub max_inline_bytes: Option<u64>,
    pub max_inline_lines: Option<u32>,
    pub verbosity: Option<ToolOutputVerbosity>,
    pub granularity: Option<ToolOutputGranularity>,
    pub oversize_policy: Option<OversizeOutputPolicy>,
}

impl ToolOutputOverrides {
    pub fn is_empty(&self) -> bool {
        self.format.is_none()
            && self.include_policy.is_none()
            && self.include_diagnostics.is_none()
            && self.include_cost.is_none()
            && self.max_inline_bytes.is_none()
            && self.max_inline_lines.is_none()
            && self.verbosity.is_none()
            && self.granularity.is_none()
            && self.oversize_policy.is_none()
    }

    fn apply_to(&self, defaults: &mut ToolOutputDefaults) {
        if let Some(format) = self.format {
            defaults.render_options.format = format;
        }
        if let Some(include_policy) = self.include_policy {
            defaults.render_options.include_policy = include_policy;
        }
        if let Some(include_diagnostics) = self.include_diagnostics {
            defaults.render_options.include_diagnostics = include_diagnostics;
        }
        if let Some(include_cost) = self.include_cost {
            defaults.render_options.include_cost = include_cost;
        }
        if let Some(max_inline_bytes) = self.max_inline_bytes {
            defaults.max_inline_bytes = max_inline_bytes;
        }
        if let Some(max_inline_lines) = self.max_inline_lines {
            defaults.max_inline_lines = max_inline_lines;
        }
        if let Some(verbosity) = self.verbosity {
            defaults.verbosity = verbosity;
        }
        if let Some(granularity) = self.granularity {
            defaults.granularity = granularity;
        }
        if let Some(oversize_policy) = self.oversize_policy {
            defaults.oversize_policy = oversize_policy;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputSettings {
    pub schema_version: u32,
    pub scope: ToolOutputSettingsScope,
    pub defaults: ToolOutputDefaults,
    pub updated_at: LedgerTimestamp,
    pub updated_by: Option<Actor>,
}

impl ToolOutputSettings {
    pub fn new(scope: ToolOutputSettingsScope, created_at: LedgerTimestamp) -> Self {
        Self {
            schema_version: TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            scope,
            defaults: ToolOutputDefaults::default(),
            updated_at: created_at,
            updated_by: None,
        }
    }

    pub fn apply_update(
        &mut self,
        actor: Actor,
        update: ToolOutputOverrides,
        reason: impl Into<String>,
        updated_at: LedgerTimestamp,
    ) -> Result<ToolOutputSettingsChange, ToolOutputSettingsError> {
        if update.is_empty() {
            return Err(ToolOutputSettingsError::EmptyUpdate);
        }

        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ToolOutputSettingsError::MissingReason);
        }

        let old_defaults = self.defaults.clone();
        let new_defaults = old_defaults.with_overrides(&update)?;

        self.defaults = new_defaults.clone();
        self.updated_at = updated_at;
        self.updated_by = Some(actor.clone());

        Ok(ToolOutputSettingsChange {
            schema_version: TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            actor,
            scope: self.scope.clone(),
            old_defaults,
            new_defaults,
            reason,
            changed_at: updated_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputSettingsChange {
    pub schema_version: u32,
    pub actor: Actor,
    pub scope: ToolOutputSettingsScope,
    pub old_defaults: ToolOutputDefaults,
    pub new_defaults: ToolOutputDefaults,
    pub reason: String,
    pub changed_at: LedgerTimestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputSettingsError {
    EmptyUpdate,
    MissingReason,
    MaxInlineBytesOutOfRange { value: u64, min: u64, max: u64 },
    MaxInlineLinesOutOfRange { value: u32, min: u32, max: u32 },
}

impl fmt::Display for ToolOutputSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUpdate => {
                write!(f, "tool output settings update must set at least one field")
            }
            Self::MissingReason => write!(f, "tool output settings update requires a reason"),
            Self::MaxInlineBytesOutOfRange { value, min, max } => write!(
                f,
                "max_inline_bytes {value} is outside the supported range {min}..={max}"
            ),
            Self::MaxInlineLinesOutOfRange { value, min, max } => write!(
                f,
                "max_inline_lines {value} is outside the supported range {min}..={max}"
            ),
        }
    }
}

impl Error for ToolOutputSettingsError {}

fn validate_inline_bytes(value: u64) -> Result<(), ToolOutputSettingsError> {
    if (MIN_MAX_INLINE_BYTES..=MAX_MAX_INLINE_BYTES).contains(&value) {
        Ok(())
    } else {
        Err(ToolOutputSettingsError::MaxInlineBytesOutOfRange {
            value,
            min: MIN_MAX_INLINE_BYTES,
            max: MAX_MAX_INLINE_BYTES,
        })
    }
}

fn validate_inline_lines(value: u32) -> Result<(), ToolOutputSettingsError> {
    if (MIN_MAX_INLINE_LINES..=MAX_MAX_INLINE_LINES).contains(&value) {
        Ok(())
    } else {
        Err(ToolOutputSettingsError::MaxInlineLinesOutOfRange {
            value,
            min: MIN_MAX_INLINE_LINES,
            max: MAX_MAX_INLINE_LINES,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    #[test]
    fn defaults_are_token_efficient_and_map_to_render_options() {
        let defaults = ToolOutputDefaults::default();
        let render_options = defaults.render_options();

        assert_eq!(render_options.format, OutputFormat::Toon);
        assert!(!render_options.include_policy);
        assert!(!render_options.include_diagnostics);
        assert!(!render_options.include_cost);
        assert_eq!(defaults.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
        assert_eq!(defaults.max_inline_lines, DEFAULT_MAX_INLINE_LINES);
        assert_eq!(defaults.verbosity, ToolOutputVerbosity::Normal);
        assert_eq!(defaults.granularity, ToolOutputGranularity::KeyFields);
        assert_eq!(
            defaults.oversize_policy,
            OversizeOutputPolicy::TruncateWithMetadata
        );
        assert!(defaults.validate().is_ok());
    }

    #[test]
    fn single_call_overrides_do_not_mutate_defaults() {
        let defaults = ToolOutputDefaults::default();
        let adjusted = defaults
            .with_overrides(&ToolOutputOverrides {
                format: Some(OutputFormat::Json),
                include_diagnostics: Some(true),
                max_inline_bytes: Some(32 * 1024),
                verbosity: Some(ToolOutputVerbosity::Expanded),
                ..ToolOutputOverrides::default()
            })
            .unwrap();

        assert_eq!(defaults.render_options.format, OutputFormat::Toon);
        assert!(!defaults.render_options.include_diagnostics);
        assert_eq!(defaults.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
        assert_eq!(adjusted.render_options.format, OutputFormat::Json);
        assert!(adjusted.render_options.include_diagnostics);
        assert_eq!(adjusted.max_inline_bytes, 32 * 1024);
        assert_eq!(adjusted.verbosity, ToolOutputVerbosity::Expanded);
    }

    #[test]
    fn settings_update_records_audit_shape() {
        let mut settings = ToolOutputSettings::new(
            ToolOutputSettingsScope::workspace().for_tool("fs.read"),
            LedgerTimestamp::from_unix_millis(1),
        );

        let change = settings
            .apply_update(
                actor(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Text),
                    include_policy: Some(true),
                    max_inline_lines: Some(80),
                    oversize_policy: Some(OversizeOutputPolicy::SpillToArtifactRef),
                    ..ToolOutputOverrides::default()
                },
                "PDH-147 tune fs.read for concise agent inspection",
                LedgerTimestamp::from_unix_millis(2),
            )
            .unwrap();

        assert_eq!(change.scope.tool_id.as_deref(), Some("fs.read"));
        assert_eq!(
            change.old_defaults.render_options.format,
            OutputFormat::Toon
        );
        assert_eq!(
            change.new_defaults.render_options.format,
            OutputFormat::Text
        );
        assert!(change.new_defaults.render_options.include_policy);
        assert_eq!(change.new_defaults.max_inline_lines, 80);
        assert_eq!(
            change.new_defaults.oversize_policy,
            OversizeOutputPolicy::SpillToArtifactRef
        );
        assert_eq!(change.changed_at, LedgerTimestamp::from_unix_millis(2));
        assert_eq!(settings.updated_at, LedgerTimestamp::from_unix_millis(2));
        assert_eq!(settings.defaults, change.new_defaults);
    }

    #[test]
    fn settings_update_rejects_unbounded_values_without_mutating() {
        let mut settings = ToolOutputSettings::new(
            ToolOutputSettingsScope::session("session-1"),
            LedgerTimestamp::from_unix_millis(1),
        );
        let old_defaults = settings.defaults.clone();

        let error = settings
            .apply_update(
                actor(),
                ToolOutputOverrides {
                    max_inline_bytes: Some(MAX_MAX_INLINE_BYTES + 1),
                    ..ToolOutputOverrides::default()
                },
                "too large",
                LedgerTimestamp::from_unix_millis(2),
            )
            .unwrap_err();

        assert_eq!(
            error,
            ToolOutputSettingsError::MaxInlineBytesOutOfRange {
                value: MAX_MAX_INLINE_BYTES + 1,
                min: MIN_MAX_INLINE_BYTES,
                max: MAX_MAX_INLINE_BYTES,
            }
        );
        assert_eq!(settings.defaults, old_defaults);
        assert_eq!(settings.updated_at, LedgerTimestamp::from_unix_millis(1));
    }

    #[test]
    fn settings_update_requires_reason_and_values() {
        let mut settings = ToolOutputSettings::new(
            ToolOutputSettingsScope::agent_profile("agent:test"),
            LedgerTimestamp::from_unix_millis(1),
        );

        assert_eq!(
            settings
                .apply_update(
                    actor(),
                    ToolOutputOverrides::default(),
                    "empty",
                    LedgerTimestamp::from_unix_millis(2),
                )
                .unwrap_err(),
            ToolOutputSettingsError::EmptyUpdate
        );

        assert_eq!(
            settings
                .apply_update(
                    actor(),
                    ToolOutputOverrides {
                        max_inline_lines: Some(40),
                        ..ToolOutputOverrides::default()
                    },
                    " ",
                    LedgerTimestamp::from_unix_millis(2),
                )
                .unwrap_err(),
            ToolOutputSettingsError::MissingReason
        );
    }

    #[test]
    fn settings_serialize_with_snake_case_enums() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Json),
            verbosity: ToolOutputVerbosity::Debug,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::RejectOversize,
            ..ToolOutputDefaults::default()
        };

        let json = serde_json::to_string(&defaults).unwrap();

        assert!(json.contains("\"format\":\"json\""));
        assert!(json.contains("\"verbosity\":\"debug\""));
        assert!(json.contains("\"granularity\":\"full\""));
        assert!(json.contains("\"oversize_policy\":\"reject_oversize\""));
    }
}
