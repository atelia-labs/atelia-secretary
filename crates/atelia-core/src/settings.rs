//! Configurable defaults for agent-facing tool output.
//!
//! These types keep rarely changed AX knobs out of individual tool call
//! schemas while still giving the runtime a validated, auditable settings
//! surface.

use crate::domain::{Actor, LedgerTimestamp, RepositoryId};
use crate::tool_output::{OutputFormat, RenderOptions, ToolOutputRenderPolicy};
use crate::ProjectId;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
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

#[derive(Debug, Clone, Default)]
pub struct InMemoryToolOutputSettingsService {
    settings: Vec<ToolOutputSettings>,
    changes: Vec<ToolOutputSettingsChange>,
}

impl InMemoryToolOutputSettingsService {
    pub fn new(created_at: LedgerTimestamp) -> Self {
        let settings = vec![ToolOutputSettings::new(
            ToolOutputSettingsScope::workspace(),
            created_at,
        )];

        Self {
            settings,
            changes: Vec::new(),
        }
    }

    pub fn new_with_settings(
        created_at: LedgerTimestamp,
        settings: Vec<ToolOutputSettings>,
    ) -> Result<Self, ToolOutputSettingsError> {
        let mut scopes = Vec::with_capacity(settings.len());
        for setting in &settings {
            if scopes.contains(&setting.scope) {
                return Err(ToolOutputSettingsError::DuplicateScope {
                    scope: setting.scope.clone(),
                });
            }
            scopes.push(setting.scope.clone());
        }

        let mut service = Self {
            settings,
            changes: Vec::new(),
        };

        if !service
            .settings
            .iter()
            .any(|setting| setting.scope == ToolOutputSettingsScope::workspace())
        {
            service.settings.push(ToolOutputSettings::new(
                ToolOutputSettingsScope::workspace(),
                created_at,
            ));
        }

        service.migrate_legacy_settings();
        Ok(service)
    }

    pub fn new_with_defaults(
        created_at: LedgerTimestamp,
        base_defaults: ToolOutputDefaults,
    ) -> Self {
        let base_overrides = ToolOutputOverrides::from_defaults(&base_defaults);

        let settings = vec![ToolOutputSettings {
            schema_version: TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            scope: ToolOutputSettingsScope::workspace(),
            overrides: base_overrides,
            updated_at: created_at,
            updated_by: None,
            legacy_defaults: None,
        }];

        Self {
            settings,
            changes: Vec::new(),
        }
    }

    pub fn apply_update(
        &mut self,
        actor: Actor,
        scope: ToolOutputSettingsScope,
        update: ToolOutputOverrides,
        reason: impl Into<String>,
        updated_at: LedgerTimestamp,
    ) -> Result<ToolOutputSettingsChange, ToolOutputSettingsError> {
        let idx = self
            .settings
            .iter()
            .position(|candidate| candidate.scope == scope);
        let change = if let Some(idx) = idx {
            let inherited = self.resolve_parent_defaults_for(&scope);
            self.settings[idx].apply_update(actor, update, reason, &inherited, updated_at)?
        } else {
            let mut settings = ToolOutputSettings::new(scope.clone(), updated_at);
            let inherited = self.resolve_parent_defaults_for(&scope);
            let change = settings.apply_update(actor, update, reason, &inherited, updated_at)?;
            self.settings.push(settings);
            change
        };

        self.changes.push(change.clone());
        Ok(change)
    }

    pub fn resolve_defaults(&self, scope: &ToolOutputSettingsScope) -> ToolOutputDefaults {
        let mut defaults = ToolOutputDefaults::default();
        for candidate_scope in resolution_chain(scope) {
            if let Some(candidate) = self
                .settings
                .iter()
                .find(|setting| setting.scope == candidate_scope)
            {
                let candidate_overrides = candidate.resolved_overrides(&defaults);
                candidate_overrides.apply_to(&mut defaults);
            }
        }
        defaults
    }

    fn resolve_parent_defaults_for(&self, scope: &ToolOutputSettingsScope) -> ToolOutputDefaults {
        let mut defaults = ToolOutputDefaults::default();
        for candidate_scope in resolution_chain(scope) {
            if candidate_scope == *scope {
                break;
            }
            if let Some(candidate) = self
                .settings
                .iter()
                .find(|setting| setting.scope == candidate_scope)
            {
                let candidate_overrides = candidate.resolved_overrides(&defaults);
                candidate_overrides.apply_to(&mut defaults);
            }
        }
        defaults
    }

    fn migrate_legacy_settings(&mut self) {
        let mut indices: Vec<usize> = (0..self.settings.len()).collect();
        indices.sort_by_key(|idx| {
            let scope = &self.settings[*idx].scope;
            (scope_migration_depth(scope), *idx)
        });

        for idx in indices {
            let scope = self.settings[idx].scope.clone();
            let inherited = self.resolve_parent_defaults_for(&scope);
            self.settings[idx].apply_legacy_migration(&inherited);
        }
    }

    pub fn resolve_render_options(&self, scope: &ToolOutputSettingsScope) -> RenderOptions {
        self.resolve_defaults(scope).render_options()
    }

    pub fn resolve_defaults_with_overrides(
        &self,
        scope: &ToolOutputSettingsScope,
        overrides: &ToolOutputOverrides,
    ) -> Result<ToolOutputDefaults, ToolOutputSettingsError> {
        self.resolve_defaults(scope).with_overrides(overrides)
    }

    pub fn changes(&self) -> &[ToolOutputSettingsChange] {
        self.changes.as_slice()
    }
}

fn resolution_chain(scope: &ToolOutputSettingsScope) -> Vec<ToolOutputSettingsScope> {
    let mut chain = Vec::new();
    let workspace_scope = ToolOutputSettingsScope::workspace();

    push_unique(&mut chain, ToolOutputSettingsScope::workspace());

    let level_scope = ToolOutputSettingsScope {
        level: scope.level.clone(),
        tool_id: None,
    };
    push_unique(&mut chain, level_scope);

    if let Some(tool_id) = scope.tool_id.as_deref() {
        push_unique(&mut chain, workspace_scope.for_tool(tool_id));
    }

    if scope.tool_id.as_deref().is_some() {
        push_unique(&mut chain, scope.clone());
    }

    chain
}

fn push_unique(chain: &mut Vec<ToolOutputSettingsScope>, scope: ToolOutputSettingsScope) {
    if !chain.contains(&scope) {
        chain.push(scope);
    }
}

fn scope_migration_depth(scope: &ToolOutputSettingsScope) -> usize {
    usize::from(scope.tool_id.is_some()) + 1
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolOutputDefaults {
    pub render_options: RenderOptions,
    /// Number of bytes kept in the "inlined" renderer path before truncation.
    /// Validation runs on deserialize/update, and runtime policy can now enforce
    /// oversize behavior against this threshold when requested.
    pub max_inline_bytes: u64,
    pub max_inline_lines: u32,
    /// Reserved for future renderer policy rollout; currently persisted and
    /// validated but not yet enforced by runtime output rendering.
    pub verbosity: ToolOutputVerbosity,
    pub granularity: ToolOutputGranularity,
    /// Runtime policy for oversized canonical fields, including artifact spillover
    /// and hard-reject behavior.
    pub oversize_policy: OversizeOutputPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolOutputDefaultsUnvalidated {
    render_options: RenderOptions,
    max_inline_bytes: u64,
    max_inline_lines: u32,
    verbosity: ToolOutputVerbosity,
    granularity: ToolOutputGranularity,
    oversize_policy: OversizeOutputPolicy,
}

impl TryFrom<ToolOutputDefaultsUnvalidated> for ToolOutputDefaults {
    type Error = ToolOutputSettingsError;

    fn try_from(value: ToolOutputDefaultsUnvalidated) -> Result<Self, Self::Error> {
        let defaults = Self {
            render_options: value.render_options,
            max_inline_bytes: value.max_inline_bytes,
            max_inline_lines: value.max_inline_lines,
            verbosity: value.verbosity,
            granularity: value.granularity,
            oversize_policy: value.oversize_policy,
        };
        defaults.validate()?;
        Ok(defaults)
    }
}

impl<'de> Deserialize<'de> for ToolOutputDefaults {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ToolOutputDefaultsUnvalidated::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl ToolOutputDefaults {
    pub fn render_options(&self) -> RenderOptions {
        self.render_options.clone()
    }

    pub fn render_policy(&self) -> ToolOutputRenderPolicy {
        self.render_policy_with_render_options(None)
    }

    pub fn render_policy_with_render_options(
        &self,
        requested_render_options: Option<&RenderOptions>,
    ) -> ToolOutputRenderPolicy {
        let mut render_options = self.render_options();
        if let Some(requested_render_options) = requested_render_options {
            render_options.format = requested_render_options.format;
            render_options.include_policy = requested_render_options.include_policy;
            render_options.include_diagnostics = requested_render_options.include_diagnostics;
            render_options.include_cost = requested_render_options.include_cost;
        }

        apply_verbosity_constraints(&mut render_options, self.verbosity);

        let max_inline_lines = usize::try_from(self.max_inline_lines).unwrap_or(usize::MAX);
        let field_limit = match self.granularity {
            ToolOutputGranularity::Summary => Some(1),
            ToolOutputGranularity::KeyFields => Some(3),
            ToolOutputGranularity::Full => None,
        }
        .map(|limit| limit.min(max_inline_lines));

        ToolOutputRenderPolicy {
            render_options,
            max_fields: field_limit,
            max_inline_lines: Some(max_inline_lines),
            include_evidence_refs: !matches!(self.granularity, ToolOutputGranularity::Summary),
            include_output_refs: !matches!(self.granularity, ToolOutputGranularity::Summary),
            include_redactions: !matches!(self.granularity, ToolOutputGranularity::Summary),
        }
    }

    fn apply_overrides(&mut self, overrides: &ToolOutputOverrides) {
        overrides.apply_to(self);
    }

    pub fn with_overrides(
        &self,
        overrides: &ToolOutputOverrides,
    ) -> Result<Self, ToolOutputSettingsError> {
        let mut defaults = self.clone();
        defaults.apply_overrides(overrides);
        defaults.validate()?;
        Ok(defaults)
    }

    pub fn validate(&self) -> Result<(), ToolOutputSettingsError> {
        validate_inline_bytes(self.max_inline_bytes)?;
        validate_inline_lines(self.max_inline_lines)?;
        Ok(())
    }
}

fn apply_verbosity_constraints(render_options: &mut RenderOptions, verbosity: ToolOutputVerbosity) {
    match verbosity {
        ToolOutputVerbosity::Minimal => {
            render_options.include_policy = false;
            render_options.include_diagnostics = false;
            render_options.include_cost = false;
        }
        ToolOutputVerbosity::Normal => {
            render_options.include_policy = false;
            render_options.include_diagnostics = false;
            render_options.include_cost = false;
        }
        ToolOutputVerbosity::Expanded => {
            render_options.include_policy = true;
            render_options.include_diagnostics = true;
            render_options.include_cost = false;
        }
        ToolOutputVerbosity::Debug => {
            render_options.include_policy = true;
            render_options.include_diagnostics = true;
            render_options.include_cost = true;
        }
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

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Deserialize)]
struct ToolOutputOverridesUnvalidated {
    format: Option<OutputFormat>,
    include_policy: Option<bool>,
    include_diagnostics: Option<bool>,
    include_cost: Option<bool>,
    max_inline_bytes: Option<u64>,
    max_inline_lines: Option<u32>,
    verbosity: Option<ToolOutputVerbosity>,
    granularity: Option<ToolOutputGranularity>,
    oversize_policy: Option<OversizeOutputPolicy>,
}

impl TryFrom<ToolOutputOverridesUnvalidated> for ToolOutputOverrides {
    type Error = ToolOutputSettingsError;

    fn try_from(value: ToolOutputOverridesUnvalidated) -> Result<Self, Self::Error> {
        if let Some(max_inline_bytes) = value.max_inline_bytes {
            validate_inline_bytes(max_inline_bytes)?;
        }
        if let Some(max_inline_lines) = value.max_inline_lines {
            validate_inline_lines(max_inline_lines)?;
        }

        Ok(Self {
            format: value.format,
            include_policy: value.include_policy,
            include_diagnostics: value.include_diagnostics,
            include_cost: value.include_cost,
            max_inline_bytes: value.max_inline_bytes,
            max_inline_lines: value.max_inline_lines,
            verbosity: value.verbosity,
            granularity: value.granularity,
            oversize_policy: value.oversize_policy,
        })
    }
}

impl<'de> Deserialize<'de> for ToolOutputOverrides {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ToolOutputOverridesUnvalidated::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
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

    fn from_defaults(base: &ToolOutputDefaults) -> Self {
        Self::from_defaults_with_parent(base, &ToolOutputDefaults::default())
    }

    fn from_defaults_with_parent(
        base: &ToolOutputDefaults,
        inherited: &ToolOutputDefaults,
    ) -> Self {
        Self {
            format: (base.render_options.format != inherited.render_options.format)
                .then_some(base.render_options.format),
            include_policy: (base.render_options.include_policy
                != inherited.render_options.include_policy)
                .then_some(base.render_options.include_policy),
            include_diagnostics: (base.render_options.include_diagnostics
                != inherited.render_options.include_diagnostics)
                .then_some(base.render_options.include_diagnostics),
            include_cost: (base.render_options.include_cost
                != inherited.render_options.include_cost)
                .then_some(base.render_options.include_cost),
            max_inline_bytes: (base.max_inline_bytes != inherited.max_inline_bytes)
                .then_some(base.max_inline_bytes),
            max_inline_lines: (base.max_inline_lines != inherited.max_inline_lines)
                .then_some(base.max_inline_lines),
            verbosity: (base.verbosity != inherited.verbosity).then_some(base.verbosity),
            granularity: (base.granularity != inherited.granularity).then_some(base.granularity),
            oversize_policy: (base.oversize_policy != inherited.oversize_policy)
                .then_some(base.oversize_policy),
        }
    }

    fn merge(&self, update: &Self) -> Self {
        Self {
            format: update.format.or(self.format),
            include_policy: update.include_policy.or(self.include_policy),
            include_diagnostics: update.include_diagnostics.or(self.include_diagnostics),
            include_cost: update.include_cost.or(self.include_cost),
            max_inline_bytes: update.max_inline_bytes.or(self.max_inline_bytes),
            max_inline_lines: update.max_inline_lines.or(self.max_inline_lines),
            verbosity: update.verbosity.or(self.verbosity),
            granularity: update.granularity.or(self.granularity),
            oversize_policy: update.oversize_policy.or(self.oversize_policy),
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputSettings {
    pub schema_version: u32,
    pub scope: ToolOutputSettingsScope,
    pub overrides: ToolOutputOverrides,
    pub updated_at: LedgerTimestamp,
    pub updated_by: Option<Actor>,
    legacy_defaults: Option<ToolOutputDefaults>,
}

#[derive(Debug, Deserialize)]
struct ToolOutputSettingsUnvalidated {
    schema_version: u32,
    scope: ToolOutputSettingsScope,
    #[serde(default)]
    overrides: Option<ToolOutputOverrides>,
    #[serde(default)]
    defaults: Option<ToolOutputDefaults>,
    updated_at: LedgerTimestamp,
    updated_by: Option<Actor>,
}

impl<'de> Deserialize<'de> for ToolOutputSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ToolOutputSettingsUnvalidated::deserialize(deserializer)?;
        let (overrides, legacy_defaults) = match (raw.overrides, raw.defaults) {
            (Some(overrides), None) => (overrides, None),
            (None, Some(defaults)) => (ToolOutputOverrides::default(), Some(defaults)),
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(
                    "settings payload must not provide both `overrides` and legacy `defaults`",
                ));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(
                    "missing required field `overrides` (or legacy `defaults`)",
                ));
            }
        };

        Ok(Self {
            schema_version: raw.schema_version,
            scope: raw.scope,
            overrides,
            updated_at: raw.updated_at,
            updated_by: raw.updated_by,
            legacy_defaults,
        })
    }
}

impl Serialize for ToolOutputSettings {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ToolOutputSettings", 5)?;
        state.serialize_field("schema_version", &self.schema_version)?;
        state.serialize_field("scope", &self.scope)?;

        if let Some(legacy_defaults) = &self.legacy_defaults {
            state.serialize_field("defaults", legacy_defaults)?;
        } else {
            state.serialize_field("overrides", &self.overrides)?;
        }

        state.serialize_field("updated_at", &self.updated_at)?;
        state.serialize_field("updated_by", &self.updated_by)?;
        state.end()
    }
}

impl ToolOutputSettings {
    fn resolved_overrides(&self, inherited: &ToolOutputDefaults) -> ToolOutputOverrides {
        match &self.legacy_defaults {
            Some(defaults) => ToolOutputOverrides::from_defaults_with_parent(defaults, inherited),
            None => self.overrides.clone(),
        }
    }

    fn apply_legacy_migration(&mut self, inherited: &ToolOutputDefaults) {
        if let Some(defaults) = self.legacy_defaults.take() {
            self.overrides = ToolOutputOverrides::from_defaults_with_parent(&defaults, inherited);
        }
    }

    pub fn new(scope: ToolOutputSettingsScope, created_at: LedgerTimestamp) -> Self {
        Self {
            schema_version: TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            scope,
            overrides: ToolOutputOverrides::default(),
            updated_at: created_at,
            updated_by: None,
            legacy_defaults: None,
        }
    }

    pub fn apply_update(
        &mut self,
        actor: Actor,
        update: ToolOutputOverrides,
        reason: impl Into<String>,
        inherited_defaults: &ToolOutputDefaults,
        updated_at: LedgerTimestamp,
    ) -> Result<ToolOutputSettingsChange, ToolOutputSettingsError> {
        if update.is_empty() {
            return Err(ToolOutputSettingsError::EmptyUpdate);
        }

        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ToolOutputSettingsError::MissingReason);
        }

        let resolved_overrides = if let Some(defaults) = self.legacy_defaults.as_ref() {
            ToolOutputOverrides::from_defaults_with_parent(defaults, inherited_defaults)
        } else {
            self.overrides.clone()
        };

        let mut old_defaults = inherited_defaults.clone();
        resolved_overrides.apply_to(&mut old_defaults);
        let new_defaults = old_defaults.with_overrides(&update)?;

        self.overrides = resolved_overrides.merge(&update);
        self.legacy_defaults = None;
        self.updated_by = Some(actor.clone());
        self.updated_at = updated_at;

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
    DuplicateScope { scope: ToolOutputSettingsScope },
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
            Self::DuplicateScope { scope } => {
                write!(f, "duplicate tool output settings scope: {scope:?}")
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

    fn valid_defaults_json() -> serde_json::Value {
        serde_json::json!({
            "render_options": {
                "format": "toon",
                "include_policy": false,
                "include_diagnostics": false,
                "include_cost": false,
            },
            "max_inline_bytes": DEFAULT_MAX_INLINE_BYTES,
            "max_inline_lines": DEFAULT_MAX_INLINE_LINES,
            "verbosity": "normal",
            "granularity": "key_fields",
            "oversize_policy": "truncate_with_metadata",
        })
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
    fn defaults_render_policy_applies_verbosity_granularity_and_limits() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 2,
            verbosity: ToolOutputVerbosity::Expanded,
            granularity: ToolOutputGranularity::Summary,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };

        let policy = defaults.render_policy();

        assert_eq!(policy.render_options.format, OutputFormat::Json);
        assert!(policy.render_options.include_policy);
        assert!(policy.render_options.include_diagnostics);
        assert!(!policy.render_options.include_cost);
        assert_eq!(policy.max_fields, Some(1));
        assert!(!policy.include_evidence_refs);
        assert!(!policy.include_output_refs);
        assert!(!policy.include_redactions);
    }

    #[test]
    fn defaults_render_policy_keeps_full_unbounded_by_inline_line_limit() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Text),
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 2,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };

        let policy = defaults.render_policy();

        assert_eq!(policy.max_fields, None);
        assert_eq!(policy.max_inline_lines, Some(2));
    }

    #[test]
    fn defaults_render_policy_overlays_request_render_options_without_bypassing_constraints() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Text,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 4,
            verbosity: ToolOutputVerbosity::Minimal,
            granularity: ToolOutputGranularity::Summary,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };

        let policy = defaults.render_policy_with_render_options(Some(&RenderOptions {
            format: OutputFormat::Json,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        }));

        assert_eq!(policy.render_options.format, OutputFormat::Json);
        assert!(!policy.render_options.include_policy);
        assert!(!policy.render_options.include_diagnostics);
        assert!(!policy.render_options.include_cost);
        assert_eq!(policy.max_fields, Some(1));
        assert!(!policy.include_evidence_refs);
        assert!(!policy.include_output_refs);
        assert!(!policy.include_redactions);
    }

    #[test]
    fn defaults_render_policy_caps_requested_optional_channels_by_verbosity() {
        let base_defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Text,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 4,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };

        let requested = RenderOptions {
            format: OutputFormat::Json,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        };

        let minimal = ToolOutputDefaults {
            verbosity: ToolOutputVerbosity::Minimal,
            ..base_defaults.clone()
        }
        .render_policy_with_render_options(Some(&requested));
        assert_eq!(minimal.render_options.format, OutputFormat::Json);
        assert!(!minimal.render_options.include_policy);
        assert!(!minimal.render_options.include_diagnostics);
        assert!(!minimal.render_options.include_cost);

        let normal = ToolOutputDefaults {
            verbosity: ToolOutputVerbosity::Normal,
            ..base_defaults.clone()
        }
        .render_policy_with_render_options(Some(&requested));
        assert!(!normal.render_options.include_policy);
        assert!(!normal.render_options.include_diagnostics);
        assert!(!normal.render_options.include_cost);

        let expanded = ToolOutputDefaults {
            verbosity: ToolOutputVerbosity::Expanded,
            ..base_defaults.clone()
        }
        .render_policy_with_render_options(Some(&requested));
        assert!(expanded.render_options.include_policy);
        assert!(expanded.render_options.include_diagnostics);
        assert!(!expanded.render_options.include_cost);

        let debug = ToolOutputDefaults {
            verbosity: ToolOutputVerbosity::Debug,
            ..base_defaults
        }
        .render_policy_with_render_options(Some(&requested));
        assert!(debug.render_options.include_policy);
        assert!(debug.render_options.include_diagnostics);
        assert!(debug.render_options.include_cost);
    }

    #[test]
    fn defaults_render_policy_without_request_render_options_uses_defaults() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: true,
                include_diagnostics: false,
                include_cost: true,
            },
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 4,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };

        let policy = defaults.render_policy_with_render_options(None);

        assert_eq!(policy.render_options.format, OutputFormat::Json);
        assert!(!policy.render_options.include_policy);
        assert!(!policy.render_options.include_diagnostics);
        assert!(!policy.render_options.include_cost);
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
                &ToolOutputDefaults::default(),
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
        assert_eq!(
            settings.overrides,
            ToolOutputOverrides {
                format: Some(OutputFormat::Text),
                include_policy: Some(true),
                max_inline_lines: Some(80),
                oversize_policy: Some(OversizeOutputPolicy::SpillToArtifactRef),
                ..ToolOutputOverrides::default()
            }
        );
    }

    #[test]
    fn settings_update_rejects_unbounded_values_without_mutating() {
        let mut settings = ToolOutputSettings::new(
            ToolOutputSettingsScope::session("session-1"),
            LedgerTimestamp::from_unix_millis(1),
        );
        let old_defaults = settings.overrides.clone();

        let error = settings
            .apply_update(
                actor(),
                ToolOutputOverrides {
                    max_inline_bytes: Some(MAX_MAX_INLINE_BYTES + 1),
                    ..ToolOutputOverrides::default()
                },
                "too large",
                &ToolOutputDefaults::default(),
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
        assert_eq!(settings.overrides, old_defaults);
        assert_eq!(settings.updated_at, LedgerTimestamp::from_unix_millis(1));
    }

    #[test]
    fn settings_apply_update_rejects_invalid_legacy_migration_without_mutating() {
        let inherited = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Toon,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: 24 * 1024,
            max_inline_lines: 111,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::KeyFields,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };
        let legacy_defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: true,
                include_diagnostics: true,
                include_cost: false,
            },
            max_inline_bytes: 32 * 1024,
            max_inline_lines: 333,
            verbosity: ToolOutputVerbosity::Expanded,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::SpillToArtifactRef,
        };
        let legacy_json = serde_json::json!({
            "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            "scope": serde_json::to_value(ToolOutputSettingsScope::workspace()).unwrap(),
            "defaults": serde_json::to_value(&legacy_defaults).unwrap(),
            "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1)).unwrap(),
            "updated_by": serde_json::Value::Null,
        });
        let mut settings = serde_json::from_value::<ToolOutputSettings>(legacy_json).unwrap();
        let old_overrides = settings.overrides.clone();
        let old_legacy_defaults = settings.legacy_defaults.clone();

        let error = settings
            .apply_update(
                actor(),
                ToolOutputOverrides {
                    max_inline_bytes: Some(MAX_MAX_INLINE_BYTES + 1),
                    ..ToolOutputOverrides::default()
                },
                "invalid legacy migration update",
                &inherited,
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
        assert_eq!(settings.overrides, old_overrides);
        assert_eq!(settings.legacy_defaults, old_legacy_defaults);
        assert_eq!(settings.updated_at, LedgerTimestamp::from_unix_millis(1));
    }

    #[test]
    fn settings_deserialization_rejects_out_of_range_bytes() {
        let mut invalid = valid_defaults_json();
        invalid["max_inline_bytes"] = serde_json::json!(MAX_MAX_INLINE_BYTES + 1);

        let error = serde_json::from_value::<ToolOutputDefaults>(invalid).unwrap_err();

        assert!(error.to_string().contains("max_inline_bytes"));
    }

    #[test]
    fn settings_deserialization_rejects_out_of_range_lines() {
        let mut invalid = valid_defaults_json();
        invalid["max_inline_lines"] = serde_json::json!(MIN_MAX_INLINE_LINES - 1);

        let error = serde_json::from_value::<ToolOutputDefaults>(invalid).unwrap_err();

        assert!(error.to_string().contains("max_inline_lines"));
    }

    #[test]
    fn overrides_deserialization_rejects_out_of_range_bytes() {
        let json = serde_json::json!({
            "max_inline_bytes": MAX_MAX_INLINE_BYTES + 1,
        });

        let error = serde_json::from_value::<ToolOutputOverrides>(json).unwrap_err();

        assert!(error.to_string().contains("max_inline_bytes"));
    }

    #[test]
    fn overrides_deserialization_rejects_out_of_range_lines() {
        let json = serde_json::json!({
            "max_inline_lines": MIN_MAX_INLINE_LINES - 1,
        });

        let error = serde_json::from_value::<ToolOutputOverrides>(json).unwrap_err();

        assert!(error.to_string().contains("max_inline_lines"));
    }

    #[test]
    fn settings_deserialization_keeps_legacy_defaults_for_roundtrip() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_inline_bytes: 32 * 1024,
            max_inline_lines: 333,
            verbosity: ToolOutputVerbosity::Expanded,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::SpillToArtifactRef,
        };
        let legacy_json = serde_json::json!({
            "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            "scope": serde_json::to_value(ToolOutputSettingsScope::workspace()).unwrap(),
            "defaults": serde_json::to_value(&defaults).unwrap(),
            "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1)).unwrap(),
            "updated_by": serde_json::Value::Null,
        });

        let settings = serde_json::from_value::<ToolOutputSettings>(legacy_json).unwrap();

        assert_eq!(
            settings.overrides,
            ToolOutputOverrides {
                ..ToolOutputOverrides::default()
            }
        );
        assert_eq!(settings.legacy_defaults, Some(defaults.clone()));

        let serialized = serde_json::to_value(&settings).unwrap();
        assert!(serialized["defaults"].is_object());
        assert!(serialized["overrides"].is_null());
        assert_eq!(
            serialized["defaults"],
            serde_json::to_value(&defaults).unwrap()
        );
        let roundtrip = serde_json::from_value::<ToolOutputSettings>(serialized).unwrap();
        assert_eq!(roundtrip.overrides, settings.overrides);
        assert_eq!(roundtrip.legacy_defaults, Some(defaults));
    }

    #[test]
    fn settings_deserialization_rejects_mixed_overrides_and_defaults() {
        let json = serde_json::json!({
            "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            "scope": serde_json::to_value(ToolOutputSettingsScope::workspace()).unwrap(),
            "overrides": serde_json::json!({
                "format": "json",
                "include_cost": true,
            }),
            "defaults": serde_json::to_value(ToolOutputDefaults::default()).unwrap(),
            "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1)).unwrap(),
            "updated_by": serde_json::Value::Null,
        });

        let error = serde_json::from_value::<ToolOutputSettings>(json).unwrap_err();

        assert!(error
            .to_string()
            .contains("must not provide both `overrides` and legacy `defaults`"));
    }

    #[test]
    fn settings_deserialization_rejects_invalid_overrides() {
        let invalid = serde_json::json!({
            "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            "scope": serde_json::to_value(ToolOutputSettingsScope::workspace()).unwrap(),
            "overrides": serde_json::json!({
                "max_inline_lines": MIN_MAX_INLINE_LINES - 1
            }),
            "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1)).unwrap(),
            "updated_by": serde_json::Value::Null,
        });

        let error = serde_json::from_value::<ToolOutputSettings>(invalid).unwrap_err();

        assert!(error.to_string().contains("max_inline_lines"));
    }

    #[test]
    fn service_new_with_settings_rejects_duplicate_scopes() {
        let created_at = LedgerTimestamp::from_unix_millis(1_700_000_000_000);
        let duplicated_scope = ToolOutputSettingsScope::repository(RepositoryId::new());
        let settings = vec![
            ToolOutputSettings::new(ToolOutputSettingsScope::workspace(), created_at),
            ToolOutputSettings::new(duplicated_scope.clone(), created_at),
            ToolOutputSettings::new(duplicated_scope.clone(), created_at),
        ];

        let error =
            InMemoryToolOutputSettingsService::new_with_settings(created_at, settings).unwrap_err();

        assert_eq!(
            error,
            ToolOutputSettingsError::DuplicateScope {
                scope: duplicated_scope
            }
        );
    }

    #[test]
    fn settings_apply_update_migrates_legacy_defaults_before_audit() {
        let inherited = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Toon,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: 24 * 1024,
            max_inline_lines: 111,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::KeyFields,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };
        let legacy_defaults = ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: true,
                include_diagnostics: true,
                include_cost: false,
            },
            max_inline_bytes: 32 * 1024,
            max_inline_lines: 333,
            verbosity: ToolOutputVerbosity::Expanded,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::SpillToArtifactRef,
        };
        let legacy_json = serde_json::json!({
            "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
            "scope": serde_json::to_value(ToolOutputSettingsScope::workspace()).unwrap(),
            "defaults": serde_json::to_value(&legacy_defaults).unwrap(),
            "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1)).unwrap(),
            "updated_by": serde_json::Value::Null,
        });
        let mut settings = serde_json::from_value::<ToolOutputSettings>(legacy_json).unwrap();

        let change = settings
            .apply_update(
                actor(),
                ToolOutputOverrides {
                    include_cost: Some(true),
                    ..ToolOutputOverrides::default()
                },
                "migrate legacy defaults before comparing audit diff",
                &inherited,
                LedgerTimestamp::from_unix_millis(2),
            )
            .unwrap();

        assert_eq!(settings.legacy_defaults, None);
        assert_eq!(
            change.old_defaults.render_options.format,
            OutputFormat::Json
        );
        assert!(change.old_defaults.render_options.include_policy);
        assert!(change.old_defaults.render_options.include_diagnostics);
        assert_eq!(change.old_defaults.verbosity, ToolOutputVerbosity::Expanded);
        assert_eq!(
            change.old_defaults.oversize_policy,
            OversizeOutputPolicy::SpillToArtifactRef
        );

        assert_eq!(
            change.new_defaults.render_options.format,
            OutputFormat::Json
        );
        assert!(change.new_defaults.render_options.include_cost);

        assert_eq!(settings.overrides.format, Some(OutputFormat::Json));
        assert_eq!(settings.overrides.include_cost, Some(true));
    }

    #[test]
    fn settings_knobs_for_follow_up_controls_are_stored_for_pr_20_17() {
        let defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Text),
            max_inline_bytes: 4096,
            max_inline_lines: 80,
            verbosity: ToolOutputVerbosity::Debug,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::RejectOversize,
        };

        let json = serde_json::to_string(&defaults).unwrap();
        let deserialized = serde_json::from_str::<ToolOutputDefaults>(&json).unwrap();

        assert_eq!(deserialized.verbosity, ToolOutputVerbosity::Debug);
        assert_eq!(deserialized.granularity, ToolOutputGranularity::Full);
        assert_eq!(
            deserialized.oversize_policy,
            OversizeOutputPolicy::RejectOversize
        );
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
                    &ToolOutputDefaults::default(),
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
                    &ToolOutputDefaults::default(),
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

    #[test]
    fn service_resolves_effective_defaults_by_precedence() {
        let repo_id = RepositoryId::new();
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_000),
        );
        let repository_scope = ToolOutputSettingsScope::repository(repo_id);
        let repository_tool_scope = repository_scope.clone().for_tool("tool.read");

        service
            .apply_update(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Json),
                    max_inline_lines: Some(77),
                    ..ToolOutputOverrides::default()
                },
                "set workspace default format",
                LedgerTimestamp::from_unix_millis(1_700_000_000_001),
            )
            .unwrap();
        service
            .apply_update(
                actor(),
                repository_scope.clone(),
                ToolOutputOverrides {
                    granularity: Some(ToolOutputGranularity::Full),
                    ..ToolOutputOverrides::default()
                },
                "make repository output full",
                LedgerTimestamp::from_unix_millis(1_700_000_000_003),
            )
            .unwrap();
        service
            .apply_update(
                actor(),
                repository_tool_scope.clone(),
                ToolOutputOverrides {
                    include_policy: Some(true),
                    verbosity: Some(ToolOutputVerbosity::Expanded),
                    ..ToolOutputOverrides::default()
                },
                "expand only read in repository",
                LedgerTimestamp::from_unix_millis(1_700_000_000_004),
            )
            .unwrap();

        let repository_tool_defaults = service.resolve_defaults(&repository_tool_scope);
        assert_eq!(
            repository_tool_defaults.render_options.format,
            OutputFormat::Json
        );
        assert!(repository_tool_defaults.render_options.include_policy);
        assert_eq!(
            repository_tool_defaults.granularity,
            ToolOutputGranularity::Full
        );
        assert_eq!(
            repository_tool_defaults.verbosity,
            ToolOutputVerbosity::Expanded
        );
        assert_eq!(repository_tool_defaults.max_inline_lines, 77);

        let repository_defaults = service.resolve_defaults(&repository_scope);
        assert_eq!(
            repository_defaults.render_options.format,
            OutputFormat::Json
        );
        assert!(!repository_defaults.render_options.include_policy);
        assert_eq!(repository_defaults.granularity, ToolOutputGranularity::Full);
        assert_eq!(repository_defaults.verbosity, ToolOutputVerbosity::Normal);
        assert_eq!(repository_defaults.max_inline_lines, 77);
    }

    #[test]
    fn service_resolves_workspace_tool_after_non_tool_level_scope() {
        let repo_id = RepositoryId::new();
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_200),
        );
        let repository_scope = ToolOutputSettingsScope::repository(repo_id);
        let repository_tool_scope = repository_scope.clone().for_tool("tool.read");

        service
            .apply_update(
                actor(),
                ToolOutputSettingsScope::workspace().for_tool("tool.read"),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Json),
                    ..ToolOutputOverrides::default()
                },
                "tool-scoped workspace default",
                LedgerTimestamp::from_unix_millis(1_700_000_000_201),
            )
            .unwrap();
        service
            .apply_update(
                actor(),
                repository_scope.clone(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Text),
                    ..ToolOutputOverrides::default()
                },
                "repository-level default",
                LedgerTimestamp::from_unix_millis(1_700_000_000_202),
            )
            .unwrap();

        let resolved_repository_tool = service.resolve_defaults(&repository_tool_scope);
        assert_eq!(
            resolved_repository_tool.render_options.format,
            OutputFormat::Json
        );

        let resolved_repository = service.resolve_defaults(&repository_scope);
        assert_eq!(
            resolved_repository.render_options.format,
            OutputFormat::Text
        );
    }

    #[test]
    fn service_call_overrides_do_not_mutate_stored_defaults() {
        let repo_id = RepositoryId::new();
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_010),
        );
        let repository_tool_scope =
            ToolOutputSettingsScope::repository(repo_id).for_tool("tool.read");

        service
            .apply_update(
                actor(),
                repository_tool_scope.clone(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Text),
                    max_inline_bytes: Some(32 * 1024),
                    ..ToolOutputOverrides::default()
                },
                "set baseline repository tool defaults",
                LedgerTimestamp::from_unix_millis(1_700_000_000_011),
            )
            .unwrap();

        let resolved = service
            .resolve_defaults_with_overrides(
                &repository_tool_scope,
                &ToolOutputOverrides {
                    include_diagnostics: Some(true),
                    max_inline_lines: Some(555),
                    ..ToolOutputOverrides::default()
                },
            )
            .unwrap();
        let current = service.resolve_defaults(&repository_tool_scope);

        assert!(resolved.render_options.include_diagnostics);
        assert_eq!(resolved.max_inline_lines, 555);
        assert_eq!(resolved.render_options.format, OutputFormat::Text);

        assert_eq!(current.render_options.format, OutputFormat::Text);
        assert_eq!(current.max_inline_bytes, 32 * 1024);
        assert!(!current.render_options.include_diagnostics);
        assert_eq!(current.max_inline_lines, DEFAULT_MAX_INLINE_LINES);
    }

    #[test]
    fn service_child_scoped_override_uses_live_parent_updates() {
        let repo_id = RepositoryId::new();
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_020),
        );
        let repository_tool_scope =
            ToolOutputSettingsScope::repository(repo_id).for_tool("tool.read");

        service
            .apply_update(
                actor(),
                repository_tool_scope.clone(),
                ToolOutputOverrides {
                    max_inline_lines: Some(123),
                    ..ToolOutputOverrides::default()
                },
                "set tool override first",
                LedgerTimestamp::from_unix_millis(1_700_000_000_021),
            )
            .unwrap();

        service
            .apply_update(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Json),
                    max_inline_bytes: Some(32_000),
                    ..ToolOutputOverrides::default()
                },
                "update workspace defaults after child exists",
                LedgerTimestamp::from_unix_millis(1_700_000_000_022),
            )
            .unwrap();

        let resolved = service.resolve_defaults(&repository_tool_scope);

        assert_eq!(resolved.render_options.format, OutputFormat::Json);
        assert_eq!(resolved.max_inline_lines, 123);
        assert_eq!(resolved.max_inline_bytes, 32_000);
    }

    #[test]
    fn service_legacy_scoped_defaults_use_parent_inheritance_after_migration() {
        let repository_id = RepositoryId::new();
        let workspace_scope = ToolOutputSettingsScope::workspace();
        let repository_scope = ToolOutputSettingsScope::repository(repository_id);
        let repository_tool_scope = repository_scope.clone().for_tool("tool.read");
        let legacy_setting = |scope: ToolOutputSettingsScope,
                              defaults: ToolOutputDefaults|
         -> ToolOutputSettings {
            let json = serde_json::json!({
                "schema_version": TOOL_OUTPUT_SETTINGS_SCHEMA_VERSION,
                "scope": serde_json::to_value(scope).unwrap(),
                "defaults": serde_json::to_value(defaults).unwrap(),
                "updated_at": serde_json::to_value(LedgerTimestamp::from_unix_millis(1_700_000_000_100)).unwrap(),
                "updated_by": serde_json::Value::Null,
            });

            serde_json::from_value::<ToolOutputSettings>(json).unwrap()
        };

        let workspace_defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: DEFAULT_MAX_INLINE_LINES,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::KeyFields,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };
        let repository_defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: DEFAULT_MAX_INLINE_LINES,
            verbosity: ToolOutputVerbosity::Normal,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };
        let repository_tool_defaults = ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_inline_lines: 123,
            verbosity: ToolOutputVerbosity::Expanded,
            granularity: ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        };
        let loaded_settings = vec![
            legacy_setting(workspace_scope.clone(), workspace_defaults),
            legacy_setting(repository_scope.clone(), repository_defaults),
            legacy_setting(repository_tool_scope.clone(), repository_tool_defaults),
        ];
        let mut service = InMemoryToolOutputSettingsService::new_with_settings(
            LedgerTimestamp::from_unix_millis(1_700_000_000_100),
            loaded_settings,
        )
        .unwrap();

        let before = service.resolve_defaults(&repository_tool_scope);
        assert_eq!(before.render_options.format, OutputFormat::Json);
        assert_eq!(before.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
        assert_eq!(before.max_inline_lines, 123);
        assert_eq!(before.verbosity, ToolOutputVerbosity::Expanded);
        assert_eq!(before.granularity, ToolOutputGranularity::Full);

        service
            .apply_update(
                actor(),
                workspace_scope,
                ToolOutputOverrides {
                    format: Some(OutputFormat::Text),
                    max_inline_bytes: Some(32_000),
                    ..ToolOutputOverrides::default()
                },
                "allow more inline bytes at workspace level",
                LedgerTimestamp::from_unix_millis(1_700_000_000_101),
            )
            .unwrap();

        let after = service.resolve_defaults(&repository_tool_scope);
        assert_eq!(after.render_options.format, OutputFormat::Text);
        assert_eq!(after.max_inline_bytes, 32_000);
        assert_eq!(after.max_inline_lines, 123);
        assert_eq!(after.verbosity, ToolOutputVerbosity::Expanded);
        assert_eq!(after.granularity, ToolOutputGranularity::Full);

        let repository_entry = service
            .settings
            .iter()
            .find(|setting| setting.scope == repository_scope)
            .unwrap();
        assert_eq!(
            repository_entry.overrides,
            ToolOutputOverrides {
                granularity: Some(ToolOutputGranularity::Full),
                ..ToolOutputOverrides::default()
            }
        );
    }

    #[test]
    fn service_rejects_invalid_updates_without_recording_audit() {
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_020),
        );
        let before = service.resolve_render_options(&ToolOutputSettingsScope::workspace());
        assert!(service.changes().is_empty());

        let err = service
            .apply_update(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_lines: Some(0),
                    ..ToolOutputOverrides::default()
                },
                "reject invalid",
                LedgerTimestamp::from_unix_millis(1_700_000_000_021),
            )
            .unwrap_err();

        assert_eq!(
            err,
            ToolOutputSettingsError::MaxInlineLinesOutOfRange {
                value: 0,
                min: MIN_MAX_INLINE_LINES,
                max: MAX_MAX_INLINE_LINES,
            }
        );
        assert!(service.changes().is_empty());
        assert_eq!(
            service.resolve_render_options(&ToolOutputSettingsScope::workspace()),
            before
        );
    }

    #[test]
    fn service_audit_log_captures_update_history() {
        let repository_id = RepositoryId::new();
        let mut service = InMemoryToolOutputSettingsService::new(
            LedgerTimestamp::from_unix_millis(1_700_000_000_030),
        );
        let repository_scope = ToolOutputSettingsScope::repository(repository_id);

        let first_change = service
            .apply_update(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    format: Some(OutputFormat::Text),
                    ..ToolOutputOverrides::default()
                },
                "tooling default adjustment",
                LedgerTimestamp::from_unix_millis(1_700_000_000_031),
            )
            .unwrap();

        let second_change = service
            .apply_update(
                actor(),
                repository_scope.clone(),
                ToolOutputOverrides {
                    granularity: Some(ToolOutputGranularity::Summary),
                    ..ToolOutputOverrides::default()
                },
                "repo-level granularity tweak",
                LedgerTimestamp::from_unix_millis(1_700_000_000_032),
            )
            .unwrap();

        let log = service.changes();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], first_change);
        assert_eq!(log[1], second_change);
        assert_eq!(log[0].scope, ToolOutputSettingsScope::workspace());
        assert_eq!(log[1].scope, repository_scope);
        assert_eq!(
            log[0].new_defaults.render_options.format,
            OutputFormat::Text
        );
        assert_eq!(
            log[1].new_defaults.granularity,
            ToolOutputGranularity::Summary
        );
        assert_eq!(log[0].reason, "tooling default adjustment");
        assert_eq!(log[1].reason, "repo-level granularity tweak");
    }
}
