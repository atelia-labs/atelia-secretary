//! Backend extension manifest contract and in-memory registry.
//!
//! This module implements the first enforceable slice from
//! `docs/extensions-runtime.md`: manifest validation, provenance boundaries,
//! blocklist checks, install records with rollback pointers, and explicit
//! service provide / consume declarations. It intentionally does not execute
//! extension code yet.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::net::Ipv6Addr;
use std::str::FromStr;

pub const EXTENSION_MANIFEST_SCHEMA: &str = "atelia.extension.v1";
pub const EXTENSION_RPC_PROTOCOL: &str = "atelia-extension-rpc.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub schema: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: ExtensionPublisher,
    pub description: String,
    pub types: Vec<ExtensionKind>,
    pub compatibility: ExtensionCompatibility,
    pub entrypoints: ExtensionEntrypoints,
    #[serde(default)]
    pub permissions: BTreeMap<String, ExtensionPermission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ExtensionToolDefinition>,
    #[serde(default)]
    pub services: ExtensionServices,
    #[serde(default)]
    pub tool_output: Vec<ExtensionToolOutputDefinition>,
    #[serde(default)]
    pub hooks: Vec<ExtensionHookDefinition>,
    #[serde(default)]
    pub webhooks: Vec<ExtensionWebhookDefinition>,
    #[serde(default)]
    pub composition: ExtensionComposition,
    pub failure: ExtensionFailure,
    pub provenance: ExtensionProvenance,
    pub bundle: Option<ExtensionBundleMembership>,
    #[serde(default)]
    pub migration: ExtensionMigration,
}

impl Default for ExtensionManifest {
    fn default() -> Self {
        Self {
            schema: String::new(),
            id: String::new(),
            name: String::new(),
            version: String::new(),
            publisher: ExtensionPublisher {
                name: String::new(),
                url: None,
            },
            description: String::new(),
            types: Vec::new(),
            compatibility: ExtensionCompatibility {
                atelia_protocol: String::new(),
                atelia_secretary: String::new(),
            },
            entrypoints: ExtensionEntrypoints {
                realm: ExtensionRealm::Backend,
                runtime: ExtensionRuntime::WasmRust,
                command: None,
                image: None,
                wasm: None,
                protocol: String::new(),
            },
            permissions: BTreeMap::new(),
            tools: Vec::new(),
            services: ExtensionServices::default(),
            tool_output: Vec::new(),
            hooks: Vec::new(),
            webhooks: Vec::new(),
            composition: ExtensionComposition::default(),
            failure: ExtensionFailure {
                degrade: DegradeBehavior::ReturnUnavailable,
                retry_policy: RetryPolicy::None,
            },
            provenance: ExtensionProvenance {
                source: ProvenanceSource::Local,
                repository: None,
                source_ref: None,
                manifest_path: None,
                commit: None,
                registry_identity: None,
                lineage: None,
                publication: None,
                artifact_digest: String::new(),
                manifest_digest: String::new(),
                signature: None,
                signer: None,
            },
            bundle: None,
            migration: ExtensionMigration::default(),
        }
    }
}

impl ExtensionManifest {
    /// Validate a manifest with install-equivalent policy checks without recording it.
    pub fn validate(
        &self,
        policy: &ManifestValidationPolicy,
    ) -> ExtensionValidationResult<ValidatedExtensionManifest> {
        validate_manifest(self, policy)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPublisher {
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    Tool,
    Service,
    HookProvider,
    WebhookReceiver,
    ToolOutputCustomizer,
    Workflow,
    Notification,
    ApprovalAgent,
    Review,
    ReviewAgent,
    AgentProvider,
    #[serde(alias = "delegated_agent_provider")]
    DelegatedAgent,
    MemoryProvider,
    MemoryStrategy,
    Integration,
    #[serde(alias = "client_surface")]
    Presentation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionCompatibility {
    pub atelia_protocol: String,
    pub atelia_secretary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionEntrypoints {
    pub realm: ExtensionRealm,
    pub runtime: ExtensionRuntime,
    pub command: Option<String>,
    pub image: Option<String>,
    pub wasm: Option<String>,
    pub protocol: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRealm {
    Backend,
    Client,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRuntime {
    WasmRust,
    Wasm,
    Docker,
    Process,
    Remote,
    SwiftClient,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPermission {
    pub description: String,
    pub risk_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionToolDefinition {
    pub id: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub permissions_required: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionServices {
    pub provides: Vec<ExtensionServiceDefinition>,
    pub consumes: Vec<ExtensionServiceDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionToolOutputDefinition {
    pub tool_id: String,
    pub format: Option<String>,
    pub verbosity: Option<String>,
    pub language_mode: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub redactions: Vec<String>,
    pub include_policy: Option<bool>,
    pub include_cost: Option<bool>,
    pub include_diagnostics: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionHookDefinition {
    pub hook_id: String,
    pub trigger: Option<String>,
    pub verification: Option<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    pub action: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionWebhookDefinition {
    pub webhook_id: String,
    pub source: Option<String>,
    pub event: Option<String>,
    pub endpoint: Option<String>,
    pub verification: Option<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionComposition {
    #[serde(default)]
    pub attachments: Vec<ExtensionCompositionAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionCompositionAttachment {
    pub extension_id: String,
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionMigration {
    #[serde(default)]
    pub from: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionServiceDefinition {
    pub service: String,
    pub method: String,
    pub schema_version: String,
    pub required_permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionServiceDependency {
    pub extension_id: String,
    pub service: String,
    pub method: String,
    pub schema_version: String,
    pub required_permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionFailure {
    pub degrade: DegradeBehavior,
    pub retry_policy: RetryPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DegradeBehavior {
    DisableExtension,
    DisableFeature,
    ReturnUnavailable,
    RequireHuman,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    None,
    Bounded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionProvenance {
    pub source: ProvenanceSource,
    pub repository: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    pub commit: Option<String>,
    pub registry_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<ExtensionLineage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<ExtensionPublication>,
    pub artifact_digest: String,
    pub manifest_digest: String,
    pub signature: Option<String>,
    pub signer: Option<String>,
}

/// Lineage metadata for a package derived from another package.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionLineage {
    /// Parent package id that the current package derives from.
    pub parent_id: String,
    /// Parent package version when the derivation target is versioned.
    pub parent_version: Option<String>,
    /// Parent manifest digest when the exact source manifest is known.
    pub parent_manifest_digest: Option<String>,
    /// Relationship between the current package and its parent.
    pub relationship: ExtensionLineageRelationship,
}

/// Relationship between a derived package and its lineage parent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionLineageRelationship {
    /// User-owned remix that remains tied to its parent package identity.
    Remix,
    /// Fork that preserves source ancestry while allowing independent evolution.
    Fork,
    /// Derived package with a looser provenance relationship to the parent.
    Derived,
}

/// Publication metadata that describes registry visibility separately from source authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPublication {
    /// Current visibility state for discovery and sharing.
    pub visibility: ExtensionPublicationVisibility,
    /// Registry submission state for this package revision.
    pub registry_submission: ExtensionRegistrySubmission,
}

/// Visibility state for a package publication.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPublicationVisibility {
    /// Private remix visible only inside the user's harness or workspace.
    PrivateRemix,
    /// Shared directly without public registry searchability.
    UnlistedShare,
    /// Publicly searchable through the registry subject to submission policy.
    PublicSearchable,
    /// Official publication controlled by host policy.
    Official,
}

/// Registry submission state for package publication.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRegistrySubmission {
    /// Not submitted to a registry.
    NotSubmitted,
    /// Submitted and awaiting registry decision.
    Submitted,
    /// Accepted by registry policy.
    Accepted,
    /// Rejected by registry policy.
    Rejected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    Github,
    Registry,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionBundleMembership {
    pub id: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionBoundary {
    Official,
    ThirdParty,
    LocalDevelopment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedExtensionManifest {
    pub manifest: ExtensionManifest,
    pub boundary: ExtensionBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestValidationPolicy {
    pub allow_local_unsigned: bool,
    pub allow_local_process_runtime: bool,
    pub official_id_prefix: String,
    pub local_id_prefix: String,
    pub official_registry_identity: String,
}

impl ManifestValidationPolicy {
    pub fn with_local_unsigned(mut self) -> Self {
        self.allow_local_unsigned = true;
        self
    }

    pub fn with_local_process_runtime(mut self) -> Self {
        self.allow_local_process_runtime = true;
        self
    }
}

impl Default for ManifestValidationPolicy {
    fn default() -> Self {
        Self {
            allow_local_unsigned: false,
            allow_local_process_runtime: false,
            official_id_prefix: "ai.atelia.".to_string(),
            local_id_prefix: "local.".to_string(),
            official_registry_identity: "atelia-official".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionValidationError {
    InvalidSchema {
        expected: &'static str,
        found: String,
    },
    MissingField {
        field: &'static str,
    },
    InvalidField {
        field: &'static str,
        reason: String,
    },
    UnsupportedRuntime {
        runtime: ExtensionRuntime,
        reason: String,
    },
    ProvenanceRequired {
        field: &'static str,
        reason: String,
    },
    BoundaryViolation {
        reason: String,
    },
    DuplicateServiceDeclaration {
        service: String,
        method: String,
        schema_version: String,
    },
    MissingServicePermission {
        service: String,
        method: String,
        permission: String,
    },
    MissingToolPermission {
        tool: String,
        permission: String,
    },
    DuplicateToolDeclaration {
        tool: String,
    },
    DuplicateToolOutputDeclaration {
        tool_id: String,
    },
    DuplicateHookDeclaration {
        hook_id: String,
    },
    DuplicateWebhookDeclaration {
        webhook_id: String,
    },
}

impl fmt::Display for ExtensionValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchema { expected, found } => {
                write!(
                    f,
                    "extension manifest schema must be {expected}, found {found}"
                )
            }
            Self::MissingField { field } => {
                write!(f, "extension manifest field {field} is required")
            }
            Self::InvalidField { field, reason } => {
                write!(f, "extension manifest field {field} is invalid: {reason}")
            }
            Self::UnsupportedRuntime { runtime, reason } => {
                write!(
                    f,
                    "extension runtime {runtime:?} is not supported: {reason}"
                )
            }
            Self::ProvenanceRequired { field, reason } => {
                write!(
                    f,
                    "extension provenance field {field} is required: {reason}"
                )
            }
            Self::BoundaryViolation { reason } => {
                write!(f, "extension boundary violation: {reason}")
            }
            Self::DuplicateServiceDeclaration {
                service,
                method,
                schema_version,
            } => write!(
                f,
                "duplicate service declaration {service}.{method} schema {schema_version}"
            ),
            Self::MissingServicePermission {
                service,
                method,
                permission,
            } => write!(
                f,
                "service {service}.{method} requires undeclared permission {permission}"
            ),
            Self::MissingToolPermission { tool, permission } => {
                write!(f, "tool {tool} requires undeclared permission {permission}")
            }
            Self::DuplicateToolDeclaration { tool } => {
                write!(f, "duplicate tool declaration {tool}")
            }
            Self::DuplicateToolOutputDeclaration { tool_id } => {
                write!(f, "duplicate tool output declaration {tool_id}")
            }
            Self::DuplicateHookDeclaration { hook_id } => {
                write!(f, "duplicate hook declaration {hook_id}")
            }
            Self::DuplicateWebhookDeclaration { webhook_id } => {
                write!(f, "duplicate webhook declaration {webhook_id}")
            }
        }
    }
}

impl Error for ExtensionValidationError {}

pub type ExtensionValidationResult<T> = Result<T, ExtensionValidationError>;

fn validate_manifest(
    manifest: &ExtensionManifest,
    policy: &ManifestValidationPolicy,
) -> ExtensionValidationResult<ValidatedExtensionManifest> {
    if manifest.schema != EXTENSION_MANIFEST_SCHEMA {
        return Err(ExtensionValidationError::InvalidSchema {
            expected: EXTENSION_MANIFEST_SCHEMA,
            found: manifest.schema.clone(),
        });
    }

    require_non_empty("id", &manifest.id)?;
    require_reverse_dns_id("id", &manifest.id)?;
    require_non_empty("name", &manifest.name)?;
    require_non_empty("version", &manifest.version)?;
    require_semver("version", &manifest.version)?;
    require_non_empty("publisher.name", &manifest.publisher.name)?;
    require_non_empty("description", &manifest.description)?;
    require_non_empty(
        "compatibility.atelia_protocol",
        &manifest.compatibility.atelia_protocol,
    )?;
    require_non_empty(
        "compatibility.atelia_secretary",
        &manifest.compatibility.atelia_secretary,
    )?;

    if manifest.types.is_empty() {
        return Err(ExtensionValidationError::MissingField { field: "types" });
    }

    let boundary = classify_boundary(manifest, policy)?;
    validate_entrypoint(manifest, boundary, policy)?;
    validate_provenance(manifest, boundary, policy)?;
    validate_permissions(&manifest.permissions)?;
    validate_tools(manifest)?;
    validate_services(manifest)?;
    validate_tool_output(manifest)?;
    validate_hooks(manifest, boundary)?;
    validate_webhooks(manifest, boundary)?;
    validate_composition(manifest)?;
    validate_migration(manifest)?;

    Ok(ValidatedExtensionManifest {
        manifest: manifest.clone(),
        boundary,
    })
}

fn classify_boundary(
    manifest: &ExtensionManifest,
    policy: &ManifestValidationPolicy,
) -> ExtensionValidationResult<ExtensionBoundary> {
    let is_official_id = manifest.id.starts_with(&policy.official_id_prefix);
    let is_local_id = manifest.id.starts_with(&policy.local_id_prefix);

    match manifest.provenance.source {
        ProvenanceSource::Local => {
            if !is_local_id {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason: format!(
                        "local extensions must use the {} id namespace",
                        policy.local_id_prefix
                    ),
                });
            }
            Ok(ExtensionBoundary::LocalDevelopment)
        }
        ProvenanceSource::Registry | ProvenanceSource::Github if is_official_id => {
            if manifest.provenance.registry_identity.as_deref()
                != Some(policy.official_registry_identity.as_str())
            {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason: "official extensions must be issued by the official registry identity"
                        .to_string(),
                });
            }
            Ok(ExtensionBoundary::Official)
        }
        ProvenanceSource::Registry | ProvenanceSource::Github => {
            if is_local_id {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason: "non-local extensions cannot use the local id namespace".to_string(),
                });
            }
            Ok(ExtensionBoundary::ThirdParty)
        }
    }
}

fn validate_entrypoint(
    manifest: &ExtensionManifest,
    boundary: ExtensionBoundary,
    policy: &ManifestValidationPolicy,
) -> ExtensionValidationResult<()> {
    if manifest.entrypoints.realm != ExtensionRealm::Backend {
        return Err(ExtensionValidationError::UnsupportedRuntime {
            runtime: manifest.entrypoints.runtime,
            reason: "this registry slice only accepts backend extensions".to_string(),
        });
    }

    if manifest.entrypoints.protocol != EXTENSION_RPC_PROTOCOL {
        return Err(ExtensionValidationError::InvalidField {
            field: "entrypoints.protocol",
            reason: format!("expected {EXTENSION_RPC_PROTOCOL}"),
        });
    }

    match manifest.entrypoints.runtime {
        ExtensionRuntime::WasmRust | ExtensionRuntime::Wasm => {
            if manifest
                .entrypoints
                .wasm
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(ExtensionValidationError::MissingField {
                    field: "entrypoints.wasm",
                });
            }
        }
        ExtensionRuntime::Process => {
            if boundary != ExtensionBoundary::LocalDevelopment
                || !policy.allow_local_process_runtime
            {
                return Err(ExtensionValidationError::UnsupportedRuntime {
                    runtime: ExtensionRuntime::Process,
                    reason:
                        "process runtime is local-development only and must be explicitly enabled"
                            .to_string(),
                });
            }

            if manifest
                .entrypoints
                .command
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(ExtensionValidationError::MissingField {
                    field: "entrypoints.command",
                });
            }
        }
        ExtensionRuntime::Docker | ExtensionRuntime::Remote | ExtensionRuntime::SwiftClient => {
            return Err(ExtensionValidationError::UnsupportedRuntime {
                runtime: manifest.entrypoints.runtime,
                reason: "first backend slice supports wasm-rust, wasm, and explicit local process development only"
                    .to_string(),
            });
        }
    }

    Ok(())
}

fn validate_provenance(
    manifest: &ExtensionManifest,
    boundary: ExtensionBoundary,
    policy: &ManifestValidationPolicy,
) -> ExtensionValidationResult<()> {
    require_digest(
        "provenance.artifact_digest",
        &manifest.provenance.artifact_digest,
    )?;
    require_digest(
        "provenance.manifest_digest",
        &manifest.provenance.manifest_digest,
    )?;

    match boundary {
        ExtensionBoundary::LocalDevelopment => {
            if (!has_non_empty_trimmed(manifest.provenance.signature.as_deref())
                || !has_non_empty_trimmed(manifest.provenance.signer.as_deref()))
                && !policy.allow_local_unsigned
            {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.signature",
                    reason: "local unsigned extensions require explicit dev-mode approval"
                        .to_string(),
                });
            }
        }
        ExtensionBoundary::Official | ExtensionBoundary::ThirdParty => {
            if !has_non_empty_trimmed(manifest.provenance.signature.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.signature",
                    reason: "non-local extensions must be signed".to_string(),
                });
            }
            if !has_non_empty_trimmed(manifest.provenance.signer.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.signer",
                    reason: "non-local extensions must identify a signer".to_string(),
                });
            }
        }
    }

    match manifest.provenance.source {
        ProvenanceSource::Github => {
            if !has_non_empty_trimmed(manifest.provenance.repository.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.repository",
                    reason: "github-sourced extensions must declare a repository".to_string(),
                });
            }
            if !has_non_empty_trimmed(manifest.provenance.source_ref.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.ref",
                    reason: "github-sourced extensions must declare a ref".to_string(),
                });
            }
            if !has_non_empty_trimmed(manifest.provenance.manifest_path.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.manifest_path",
                    reason: "github-sourced extensions must declare a manifest path".to_string(),
                });
            }
            if !has_non_empty_trimmed(manifest.provenance.commit.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.commit",
                    reason: "github-sourced extensions must declare a commit".to_string(),
                });
            }
        }
        ProvenanceSource::Registry => {
            if !has_non_empty_trimmed(manifest.provenance.registry_identity.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.registry_identity",
                    reason: "registry-sourced extensions must declare registry identity"
                        .to_string(),
                });
            }
        }
        ProvenanceSource::Local => {}
    }

    if let Some(lineage) = &manifest.provenance.lineage {
        require_reverse_dns_id("provenance.lineage.parent_id", &lineage.parent_id)?;
        if let Some(version) = &lineage.parent_version {
            require_semver("provenance.lineage.parent_version", version)?;
        }
        if let Some(digest) = &lineage.parent_manifest_digest {
            require_digest("provenance.lineage.parent_manifest_digest", digest)?;
        }
    }

    validate_publication(manifest, boundary, policy)?;

    Ok(())
}

fn validate_publication(
    manifest: &ExtensionManifest,
    boundary: ExtensionBoundary,
    policy: &ManifestValidationPolicy,
) -> ExtensionValidationResult<()> {
    let Some(publication) = &manifest.provenance.publication else {
        return Ok(());
    };

    match publication.visibility {
        ExtensionPublicationVisibility::PrivateRemix => {
            if publication.registry_submission != ExtensionRegistrySubmission::NotSubmitted {
                return Err(ExtensionValidationError::InvalidField {
                    field: "provenance.publication.registry_submission",
                    reason: "private remixes must not claim registry submission".to_string(),
                });
            }
        }
        ExtensionPublicationVisibility::UnlistedShare => {
            if publication.registry_submission == ExtensionRegistrySubmission::Rejected {
                return Err(ExtensionValidationError::InvalidField {
                    field: "provenance.publication.registry_submission",
                    reason: "unlisted shares cannot rely on rejected registry submission"
                        .to_string(),
                });
            }
            if matches!(
                publication.registry_submission,
                ExtensionRegistrySubmission::Submitted | ExtensionRegistrySubmission::Accepted
            ) && !has_non_empty_trimmed(manifest.provenance.registry_identity.as_deref())
            {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.registry_identity",
                    reason: "registry-submitted unlisted shares must declare registry identity"
                        .to_string(),
                });
            }
        }
        ExtensionPublicationVisibility::PublicSearchable => {
            if boundary == ExtensionBoundary::LocalDevelopment {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason:
                        "public searchable packages cannot use local-development package authority"
                            .to_string(),
                });
            }
            if !has_non_empty_trimmed(manifest.provenance.registry_identity.as_deref()) {
                return Err(ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.registry_identity",
                    reason: "public packages must declare registry identity".to_string(),
                });
            }
            if matches!(
                publication.registry_submission,
                ExtensionRegistrySubmission::NotSubmitted | ExtensionRegistrySubmission::Rejected
            ) {
                return Err(ExtensionValidationError::InvalidField {
                    field: "provenance.publication.registry_submission",
                    reason: "public packages must be submitted or accepted by the registry"
                        .to_string(),
                });
            }
        }
        ExtensionPublicationVisibility::Official => {
            if boundary != ExtensionBoundary::Official {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason: "official publication requires official package authority".to_string(),
                });
            }
            if manifest.provenance.registry_identity.as_deref()
                != Some(policy.official_registry_identity.as_str())
            {
                return Err(ExtensionValidationError::BoundaryViolation {
                    reason: "official publication must use the official registry identity"
                        .to_string(),
                });
            }
            if publication.registry_submission != ExtensionRegistrySubmission::Accepted {
                return Err(ExtensionValidationError::InvalidField {
                    field: "provenance.publication.registry_submission",
                    reason: "official packages must be accepted by the registry".to_string(),
                });
            }
        }
    }

    Ok(())
}

fn validate_permissions(
    permissions: &BTreeMap<String, ExtensionPermission>,
) -> ExtensionValidationResult<()> {
    for (permission, metadata) in permissions {
        require_permission_name(permission)?;
        require_non_empty("permissions.description", &metadata.description)?;

        if let Some(risk_tier) = &metadata.risk_tier {
            if !matches!(risk_tier.as_str(), "R0" | "R1" | "R2" | "R3" | "R4") {
                return Err(ExtensionValidationError::InvalidField {
                    field: "permissions.risk_tier",
                    reason: format!("unsupported risk tier {risk_tier}"),
                });
            }
        }
    }

    Ok(())
}

fn validate_tools(manifest: &ExtensionManifest) -> ExtensionValidationResult<()> {
    let has_tool_type = manifest.types.contains(&ExtensionKind::Tool);
    let has_tools = !manifest.tools.is_empty();

    if has_tools && !has_tool_type {
        return Err(ExtensionValidationError::InvalidField {
            field: "types",
            reason: "tool declarations require type tool".to_string(),
        });
    }

    if has_tool_type && !has_tools {
        return Err(ExtensionValidationError::MissingField { field: "tools" });
    }

    let mut seen_tool_ids = BTreeSet::new();
    for tool in &manifest.tools {
        require_non_empty("tools.id", &tool.id)?;
        if !seen_tool_ids.insert(tool.id.clone()) {
            return Err(ExtensionValidationError::DuplicateToolDeclaration {
                tool: tool.id.clone(),
            });
        }

        if tool.permissions.is_empty() && tool.permissions_required.is_empty() {
            return Err(ExtensionValidationError::MissingField {
                field: "tools.permissions",
            });
        }

        let mut required_permissions = BTreeSet::new();
        required_permissions.extend(tool.permissions.iter());
        required_permissions.extend(tool.permissions_required.iter());

        for permission in required_permissions {
            require_permission_name(permission)?;
            if !manifest.permissions.contains_key(permission) {
                return Err(ExtensionValidationError::MissingToolPermission {
                    tool: tool.id.clone(),
                    permission: permission.clone(),
                });
            }
        }
    }

    Ok(())
}

fn validate_services(manifest: &ExtensionManifest) -> ExtensionValidationResult<()> {
    let has_service_kind = manifest.types.contains(&ExtensionKind::Service);
    let declares_services =
        !manifest.services.provides.is_empty() || !manifest.services.consumes.is_empty();

    if declares_services && !has_service_kind {
        return Err(ExtensionValidationError::InvalidField {
            field: "types",
            reason: "service declarations require type service".to_string(),
        });
    }

    if has_service_kind && !declares_services {
        return Err(ExtensionValidationError::MissingField { field: "services" });
    }

    let mut provided = BTreeSet::new();
    for service in &manifest.services.provides {
        validate_service_parts(&service.service, &service.method, &service.schema_version)?;
        require_permission_name(&service.required_permission)?;
        require_declared_service_permission(
            &manifest.permissions,
            &service.service,
            &service.method,
            &service.required_permission,
        )?;

        let key = service_key(&service.service, &service.method, &service.schema_version);
        if !provided.insert(key) {
            return Err(ExtensionValidationError::DuplicateServiceDeclaration {
                service: service.service.clone(),
                method: service.method.clone(),
                schema_version: service.schema_version.clone(),
            });
        }
    }

    let mut consumed = BTreeSet::new();
    for dependency in &manifest.services.consumes {
        require_reverse_dns_id("services.consumes.extension_id", &dependency.extension_id)?;
        validate_service_parts(
            &dependency.service,
            &dependency.method,
            &dependency.schema_version,
        )?;
        require_permission_name(&dependency.required_permission)?;
        require_declared_service_permission(
            &manifest.permissions,
            &dependency.service,
            &dependency.method,
            &dependency.required_permission,
        )?;

        let key = format!(
            "{}:{}",
            dependency.extension_id,
            service_key(
                &dependency.service,
                &dependency.method,
                &dependency.schema_version
            )
        );
        if !consumed.insert(key) {
            return Err(ExtensionValidationError::DuplicateServiceDeclaration {
                service: dependency.service.clone(),
                method: dependency.method.clone(),
                schema_version: dependency.schema_version.clone(),
            });
        }
    }

    Ok(())
}

fn validate_tool_output(manifest: &ExtensionManifest) -> ExtensionValidationResult<()> {
    let has_tool_output_kind = manifest
        .types
        .contains(&ExtensionKind::ToolOutputCustomizer);
    let declares_tool_output = !manifest.tool_output.is_empty();

    if declares_tool_output && !has_tool_output_kind {
        return Err(ExtensionValidationError::InvalidField {
            field: "types",
            reason: "tool output declarations require type tool_output_customizer".to_string(),
        });
    }

    let declared_tools = manifest
        .tools
        .iter()
        .map(|tool| tool.id.as_str())
        .collect::<BTreeSet<_>>();

    let mut seen_tool_output_ids = BTreeSet::new();

    for tool_output in &manifest.tool_output {
        require_trimmed_non_empty("tool_output.tool_id", &tool_output.tool_id)?;
        if !seen_tool_output_ids.insert(tool_output.tool_id.clone()) {
            return Err(ExtensionValidationError::DuplicateToolOutputDeclaration {
                tool_id: tool_output.tool_id.clone(),
            });
        }

        validate_optional_choice(
            "tool_output.format",
            tool_output.format.as_deref(),
            &["toon", "json"],
        )?;
        validate_optional_choice(
            "tool_output.verbosity",
            tool_output.verbosity.as_deref(),
            &["minimal", "normal", "expanded", "debug"],
        )?;
        validate_optional_choice(
            "tool_output.language_mode",
            tool_output.language_mode.as_deref(),
            &["user", "english_agent", "mixed"],
        )?;
        validate_string_list("tool_output.fields", &tool_output.fields)?;
        validate_string_list("tool_output.redactions", &tool_output.redactions)?;

        if !declared_tools.contains(tool_output.tool_id.as_str()) {
            return Err(ExtensionValidationError::InvalidField {
                field: "tool_output.tool_id",
                reason: format!(
                    "tool output customization targets undeclared tool {}",
                    tool_output.tool_id
                ),
            });
        }
    }

    Ok(())
}

fn validate_hooks(
    manifest: &ExtensionManifest,
    boundary: ExtensionBoundary,
) -> ExtensionValidationResult<()> {
    let has_hook_provider_kind = manifest.types.contains(&ExtensionKind::HookProvider);
    let declares_hooks = !manifest.hooks.is_empty();

    if declares_hooks && !has_hook_provider_kind {
        return Err(ExtensionValidationError::InvalidField {
            field: "types",
            reason: "hook declarations require type hook_provider".to_string(),
        });
    }

    let mut seen_hook_ids = BTreeSet::new();

    for hook in &manifest.hooks {
        require_trimmed_non_empty("hooks.hook_id", &hook.hook_id)?;
        if !seen_hook_ids.insert(hook.hook_id.clone()) {
            return Err(ExtensionValidationError::DuplicateHookDeclaration {
                hook_id: hook.hook_id.clone(),
            });
        }
        validate_required_string("hooks.trigger", hook.trigger.as_deref())?;
        validate_optional_choice(
            "hooks.verification",
            hook.verification.as_deref(),
            &["hmac", "github_signature", "oidc", "none_for_local_only"],
        )?;
        if matches!(hook.verification.as_deref(), Some("none_for_local_only"))
            && boundary != ExtensionBoundary::LocalDevelopment
        {
            return Err(ExtensionValidationError::InvalidField {
                field: "hooks.verification",
                reason: "none_for_local_only is only allowed for local-development manifests"
                    .to_string(),
            });
        }
        validate_string_list("hooks.required_capabilities", &hook.required_capabilities)?;
        for capability in &hook.required_capabilities {
            require_permission_name(capability)?;
        }
        validate_optional_choice(
            "hooks.action",
            hook.action.as_deref(),
            &[
                "workflow",
                "tool",
                "notification",
                "memory_update",
                "extension_action",
            ],
        )?;
        validate_optional_choice(
            "hooks.status",
            hook.status.as_deref(),
            &["enabled", "disabled", "blocked", "needs_approval"],
        )?;
    }

    Ok(())
}

fn validate_webhooks(
    manifest: &ExtensionManifest,
    boundary: ExtensionBoundary,
) -> ExtensionValidationResult<()> {
    let has_webhook_receiver_kind = manifest.types.contains(&ExtensionKind::WebhookReceiver);
    let declares_webhooks = !manifest.webhooks.is_empty();

    if declares_webhooks && !has_webhook_receiver_kind {
        return Err(ExtensionValidationError::InvalidField {
            field: "types",
            reason: "webhook declarations require type webhook_receiver".to_string(),
        });
    }

    let mut seen_webhook_ids = BTreeSet::new();

    for webhook in &manifest.webhooks {
        require_trimmed_non_empty("webhooks.webhook_id", &webhook.webhook_id)?;
        if !seen_webhook_ids.insert(webhook.webhook_id.clone()) {
            return Err(ExtensionValidationError::DuplicateWebhookDeclaration {
                webhook_id: webhook.webhook_id.clone(),
            });
        }
        validate_optional_choice(
            "webhooks.source",
            webhook.source.as_deref(),
            &["atelia", "github", "external"],
        )?;
        validate_required_string("webhooks.event", webhook.event.as_deref())?;
        validate_http_endpoint("webhooks.endpoint", webhook.endpoint.as_deref())?;
        validate_optional_choice(
            "webhooks.verification",
            webhook.verification.as_deref(),
            &["hmac", "github_signature", "oidc", "none_for_local_only"],
        )?;
        if matches!(webhook.verification.as_deref(), Some("none_for_local_only"))
            && boundary != ExtensionBoundary::LocalDevelopment
        {
            return Err(ExtensionValidationError::InvalidField {
                field: "webhooks.verification",
                reason: "none_for_local_only is only allowed for local-development manifests"
                    .to_string(),
            });
        }
        validate_string_list(
            "webhooks.required_capabilities",
            &webhook.required_capabilities,
        )?;
        for capability in &webhook.required_capabilities {
            require_permission_name(capability)?;
        }
        validate_optional_choice(
            "webhooks.status",
            webhook.status.as_deref(),
            &["enabled", "disabled", "blocked", "needs_approval"],
        )?;
    }

    Ok(())
}

fn validate_composition(manifest: &ExtensionManifest) -> ExtensionValidationResult<()> {
    for attachment in &manifest.composition.attachments {
        require_reverse_dns_id(
            "composition.attachments.extension_id",
            &attachment.extension_id,
        )?;
    }

    Ok(())
}

fn validate_migration(manifest: &ExtensionManifest) -> ExtensionValidationResult<()> {
    for version in &manifest.migration.from {
        require_semver("migration.from", version)?;
    }

    Ok(())
}

fn validate_required_string(
    field: &'static str,
    value: Option<&str>,
) -> ExtensionValidationResult<()> {
    let Some(value) = value else {
        return Err(ExtensionValidationError::MissingField { field });
    };

    require_non_empty(field, value)?;
    if value.chars().any(char::is_whitespace) {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must not contain whitespace".to_string(),
        });
    }

    Ok(())
}

fn validate_optional_choice(
    field: &'static str,
    value: Option<&str>,
    allowed: &[&str],
) -> ExtensionValidationResult<()> {
    let Some(value) = value else {
        return Ok(());
    };

    require_non_empty(field, value)?;
    if value.chars().any(char::is_whitespace) {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must not contain whitespace".to_string(),
        });
    }
    if !allowed.contains(&value) {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: format!("must be one of: {}", allowed.join(", ")),
        });
    }

    Ok(())
}

fn validate_http_endpoint(
    field: &'static str,
    value: Option<&str>,
) -> ExtensionValidationResult<()> {
    let Some(value) = value else {
        return Err(ExtensionValidationError::MissingField { field });
    };

    require_non_empty(field, value)?;
    if value.chars().any(char::is_whitespace) {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must not contain whitespace".to_string(),
        });
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must start with https://".to_string(),
        });
    };

    validate_https_endpoint_authority(field, rest)
}

fn validate_https_endpoint_authority(
    field: &'static str,
    value: &str,
) -> ExtensionValidationResult<()> {
    let authority = value.split(['/', '?', '#']).next().unwrap_or_default();

    if authority.is_empty() {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must include a host".to_string(),
        });
    }

    if authority.contains('@') {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must not contain userinfo".to_string(),
        });
    }

    let host = if let Some(stripped) = authority.strip_prefix('[') {
        let Some((host, remainder)) = stripped.split_once(']') else {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "must contain a closing ] for IPv6 hosts".to_string(),
            });
        };
        if host.is_empty() {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "must include a host".to_string(),
            });
        }
        if Ipv6Addr::from_str(host).is_err() {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "must contain a valid IPv6 host inside brackets".to_string(),
            });
        }
        if !remainder.is_empty() {
            let Some(port) = remainder.strip_prefix(':') else {
                return Err(ExtensionValidationError::InvalidField {
                    field,
                    reason: "must separate the host and path with /".to_string(),
                });
            };
            if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
                return Err(ExtensionValidationError::InvalidField {
                    field,
                    reason: "port must be numeric".to_string(),
                });
            }
        }
        return Ok(());
    } else {
        authority
    };

    let (host, port) = match host.rsplit_once(':') {
        Some((candidate_host, candidate_port))
            if !candidate_host.contains(':') && !candidate_port.is_empty() =>
        {
            (candidate_host, Some(candidate_port))
        }
        _ => (host, None),
    };

    if host.is_empty() || host.starts_with('.') || host.ends_with('.') {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must include a valid host".to_string(),
        });
    }

    if host
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
    {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "host contains invalid characters".to_string(),
        });
    }

    if let Some(port) = port {
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "port must be numeric".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_string_list(field: &'static str, values: &[String]) -> ExtensionValidationResult<()> {
    for value in values {
        require_non_empty(field, value)?;
        if value.chars().any(char::is_whitespace) {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "must not contain whitespace".to_string(),
            });
        }
    }

    Ok(())
}

fn require_declared_service_permission(
    permissions: &BTreeMap<String, ExtensionPermission>,
    service: &str,
    method: &str,
    permission: &str,
) -> ExtensionValidationResult<()> {
    if permissions.contains_key(permission) {
        Ok(())
    } else {
        Err(ExtensionValidationError::MissingServicePermission {
            service: service.to_string(),
            method: method.to_string(),
            permission: permission.to_string(),
        })
    }
}

fn validate_service_parts(
    service: &str,
    method: &str,
    schema_version: &str,
) -> ExtensionValidationResult<()> {
    require_non_empty("services.service", service)?;
    require_non_empty("services.method", method)?;
    require_non_empty("services.schema_version", schema_version)?;

    for (field, value) in [
        ("services.service", service),
        ("services.method", method),
        ("services.schema_version", schema_version),
    ] {
        if value.chars().any(char::is_whitespace) {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason: "must not contain whitespace".to_string(),
            });
        }
    }

    Ok(())
}

fn service_key(service: &str, method: &str, schema_version: &str) -> String {
    format!("{service}:{method}:{schema_version}")
}

fn require_non_empty(field: &'static str, value: &str) -> ExtensionValidationResult<()> {
    if value.trim().is_empty() {
        Err(ExtensionValidationError::MissingField { field })
    } else {
        Ok(())
    }
}

fn require_trimmed_non_empty(field: &'static str, value: &str) -> ExtensionValidationResult<()> {
    require_non_empty(field, value)?;
    if value.trim() != value {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must not contain surrounding whitespace".to_string(),
        });
    }
    Ok(())
}

fn has_non_empty_trimmed(value: Option<&str>) -> bool {
    !value.unwrap_or_default().trim().is_empty()
}

fn require_reverse_dns_id(field: &'static str, value: &str) -> ExtensionValidationResult<()> {
    if value.starts_with('.') || value.ends_with('.') || !value.contains('.') {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must use a reverse-DNS-like dotted namespace".to_string(),
        });
    }

    for segment in value.split('.') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            || !segment
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_lowercase())
        {
            return Err(ExtensionValidationError::InvalidField {
                field,
                reason:
                    "segments must start with a lowercase ascii letter and contain lowercase ascii, digits, or hyphen"
                        .to_string(),
            });
        }
    }

    Ok(())
}

fn require_semver(field: &'static str, value: &str) -> ExtensionValidationResult<()> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || part.chars().any(|ch| !ch.is_ascii_digit()))
    {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must be major.minor.patch numeric semver".to_string(),
        });
    }

    Ok(())
}

fn require_digest(field: &'static str, value: &str) -> ExtensionValidationResult<()> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must start with sha256:".to_string(),
        });
    };

    if hex.len() != 64 || hex.chars().any(|ch| !ch.is_ascii_hexdigit()) {
        return Err(ExtensionValidationError::InvalidField {
            field,
            reason: "must contain 64 hex characters after sha256:".to_string(),
        });
    }

    Ok(())
}

fn require_permission_name(value: &str) -> ExtensionValidationResult<()> {
    require_non_empty("permission", value)?;

    if value.chars().any(char::is_whitespace) {
        return Err(ExtensionValidationError::InvalidField {
            field: "permission",
            reason: "must not contain whitespace".to_string(),
        });
    }

    if !value.contains('.') && !value.contains(':') {
        return Err(ExtensionValidationError::InvalidField {
            field: "permission",
            reason: "must include a namespace separator".to_string(),
        });
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRegistry {
    manifests: BTreeMap<String, BTreeMap<String, ExtensionManifest>>,
    records: BTreeMap<String, BTreeMap<String, ExtensionInstallRecord>>,
    active_versions: BTreeMap<String, String>,
    blocklist: Vec<BlocklistEntry>,
    validation_policy: ManifestValidationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExtensionRegistrySnapshot {
    pub manifests: BTreeMap<String, BTreeMap<String, ExtensionManifest>>,
    pub records: BTreeMap<String, BTreeMap<String, ExtensionInstallRecord>>,
    pub active_versions: BTreeMap<String, String>,
    pub blocklist: Vec<BlocklistEntry>,
}

impl ExtensionRegistrySnapshot {
    pub fn validate(&self) -> Result<(), String> {
        validate_extension_registry_snapshot(self)
    }

    pub fn validate_with_policy(&self, policy: &ManifestValidationPolicy) -> Result<(), String> {
        validate_extension_registry_snapshot(self)?;
        validate_extension_registry_snapshot_manifests(self, policy)
    }
}

impl ExtensionRegistry {
    pub fn new(validation_policy: ManifestValidationPolicy) -> Self {
        Self {
            manifests: BTreeMap::new(),
            records: BTreeMap::new(),
            active_versions: BTreeMap::new(),
            blocklist: Vec::new(),
            validation_policy,
        }
    }

    pub fn from_snapshot(
        snapshot: ExtensionRegistrySnapshot,
        validation_policy: ManifestValidationPolicy,
    ) -> Result<Self, String> {
        snapshot.validate_with_policy(&validation_policy)?;
        Ok(Self {
            manifests: snapshot.manifests,
            records: snapshot.records,
            active_versions: snapshot.active_versions,
            blocklist: snapshot.blocklist,
            validation_policy,
        })
    }

    pub fn snapshot(&self) -> ExtensionRegistrySnapshot {
        ExtensionRegistrySnapshot {
            manifests: self.manifests.clone(),
            records: self.records.clone(),
            active_versions: self.active_versions.clone(),
            blocklist: self.blocklist.clone(),
        }
    }

    pub fn validation_policy(&self) -> ManifestValidationPolicy {
        self.validation_policy.clone()
    }

    pub fn in_memory() -> Self {
        Self::new(ManifestValidationPolicy::default())
    }

    pub fn add_blocklist_entry(&mut self, entry: BlocklistEntry) -> RegistryResult<()> {
        if matches!(entry.key, BlockKey::VulnerabilityId(_)) {
            return Err(RegistryError::UnsupportedBlocklistKey { key: entry.key });
        }

        self.blocklist.push(entry);
        Ok(())
    }

    fn run_validation_checks(
        &self,
        manifest: ExtensionManifest,
        options: InstallOptions,
    ) -> RegistryResult<ValidatedExtensionManifest> {
        let mut validation_policy = self.validation_policy.clone();
        if let Some(approve_local_unsigned) = options.approve_local_unsigned {
            validation_policy.allow_local_unsigned = approve_local_unsigned;
        }
        if let Some(allow_local_process_runtime) = options.allow_local_process_runtime {
            validation_policy.allow_local_process_runtime = allow_local_process_runtime;
        }

        let validated = manifest.validate(&validation_policy)?;
        self.ensure_not_blocked(&validated.manifest)?;
        self.ensure_same_version_digest_is_stable(&validated.manifest)?;
        self.ensure_source_change_is_approved(&validated.manifest, options)?;
        Ok(validated)
    }

    pub fn install(
        &mut self,
        manifest: ExtensionManifest,
        options: InstallOptions,
    ) -> RegistryResult<ExtensionInstallRecord> {
        let validated = self.run_validation_checks(manifest, options)?;

        let previous_version = self
            .active_versions
            .get(&validated.manifest.id)
            .filter(|version| *version != &validated.manifest.version)
            .cloned();
        let approved_permissions = validated
            .manifest
            .permissions
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        let record = ExtensionInstallRecord {
            id: validated.manifest.id.clone(),
            version: validated.manifest.version.clone(),
            manifest_digest: validated.manifest.provenance.manifest_digest.clone(),
            artifact_digest: validated.manifest.provenance.artifact_digest.clone(),
            source: ExtensionSourceSnapshot::from_provenance(&validated.manifest.provenance),
            boundary: validated.boundary,
            status: ExtensionInstallStatus::Installed,
            previous_version,
            approved_permissions,
            rollback_snapshot: Some(RollbackSnapshot {
                manifest_digest: validated.manifest.provenance.manifest_digest.clone(),
                artifact_digest: validated.manifest.provenance.artifact_digest.clone(),
            }),
        };

        self.manifests
            .entry(record.id.clone())
            .or_default()
            .insert(record.version.clone(), validated.manifest);
        self.records
            .entry(record.id.clone())
            .or_default()
            .insert(record.version.clone(), record.clone());
        self.active_versions
            .insert(record.id.clone(), record.version.clone());

        Ok(record)
    }

    /// Validate a manifest with the same preflight checks used by install.
    pub fn validate(
        &self,
        manifest: ExtensionManifest,
        options: InstallOptions,
    ) -> RegistryResult<ValidateExtensionManifestResponse> {
        let validated = self.run_validation_checks(manifest, options)?;

        Ok(ValidateExtensionManifestResponse {
            manifest: validated.manifest,
            boundary: validated.boundary,
        })
    }

    pub fn rollback(&mut self, extension_id: &str) -> RegistryResult<ExtensionInstallRecord> {
        let current =
            self.active_record(extension_id)
                .ok_or_else(|| RegistryError::NotInstalled {
                    extension_id: extension_id.to_string(),
                })?;
        let previous_version =
            current
                .previous_version
                .clone()
                .ok_or_else(|| RegistryError::RollbackUnavailable {
                    extension_id: extension_id.to_string(),
                })?;

        let previous_manifest = self
            .manifests
            .get(extension_id)
            .and_then(|records| records.get(&previous_version))
            .ok_or_else(|| RegistryError::RollbackUnavailable {
                extension_id: extension_id.to_string(),
            })?;
        self.ensure_not_blocked(previous_manifest)?;

        let previous_record = self
            .records
            .get_mut(extension_id)
            .and_then(|records| records.get_mut(&previous_version))
            .ok_or_else(|| RegistryError::RollbackUnavailable {
                extension_id: extension_id.to_string(),
            })?;

        self.active_versions
            .insert(extension_id.to_string(), previous_version.clone());
        previous_record.status = ExtensionInstallStatus::InstalledPreviousVersion;

        Ok(previous_record.clone())
    }

    pub fn disable(&mut self, extension_id: &str) -> RegistryResult<ExtensionInstallRecord> {
        let version = self
            .active_versions
            .get(extension_id)
            .cloned()
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;
        let record = self
            .records
            .get_mut(extension_id)
            .and_then(|records| records.get_mut(&version))
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;

        record.status = ExtensionInstallStatus::Disabled;
        Ok(record.clone())
    }

    pub fn enable(&mut self, extension_id: &str) -> RegistryResult<ExtensionInstallRecord> {
        let version = self
            .active_versions
            .get(extension_id)
            .cloned()
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;
        let manifest = self
            .manifests
            .get(extension_id)
            .and_then(|records| records.get(&version))
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;
        self.ensure_not_blocked(manifest)?;

        let record = self
            .records
            .get_mut(extension_id)
            .and_then(|records| records.get_mut(&version))
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;

        record.status = ExtensionInstallStatus::Installed;
        Ok(record.clone())
    }

    pub fn remove(&mut self, extension_id: &str) -> RegistryResult<ExtensionInstallRecord> {
        let version = self
            .active_versions
            .get(extension_id)
            .cloned()
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            })?;

        if self
            .records
            .get(extension_id)
            .and_then(|records| records.get(&version))
            .is_none()
        {
            return Err(RegistryError::NotInstalled {
                extension_id: extension_id.to_string(),
            });
        }

        self.active_versions.remove(extension_id);
        let record = self
            .records
            .get_mut(extension_id)
            .and_then(|records| records.get_mut(&version))
            .expect("record existence checked before active version removal");

        record.status = ExtensionInstallStatus::Disabled;
        Ok(record.clone())
    }

    pub fn active_record(&self, extension_id: &str) -> Option<ExtensionInstallRecord> {
        let version = self.active_versions.get(extension_id)?;
        self.records.get(extension_id)?.get(version).cloned()
    }

    pub fn extension_status(&self, extension_id: &str) -> RegistryResult<ExtensionStatusSnapshot> {
        let manifest =
            self.active_manifest(extension_id)
                .ok_or_else(|| RegistryError::NotInstalled {
                    extension_id: extension_id.to_string(),
                })?;
        let mut record =
            self.active_record(extension_id)
                .ok_or_else(|| RegistryError::NotInstalled {
                    extension_id: extension_id.to_string(),
                })?;
        let block = self
            .find_blocklist_hit(manifest)
            .map(|entry| ExtensionBlocklistMatch {
                reason: entry.reason,
                key: entry.key.clone(),
            });

        if block.is_some() {
            record.status = ExtensionInstallStatus::Blocked;
        }

        Ok(ExtensionStatusSnapshot {
            extension_id: extension_id.to_string(),
            record,
            block,
        })
    }

    pub fn list_extension_statuses(&self) -> RegistryResult<Vec<ExtensionStatusSnapshot>> {
        self.active_versions
            .keys()
            .map(|extension_id| self.extension_status(extension_id))
            .collect()
    }

    pub fn blocklist_entries(&self) -> Vec<BlocklistEntry> {
        self.blocklist.clone()
    }

    pub fn authorize_service_call(
        &self,
        request: ServiceCallRequest,
    ) -> RegistryResult<ServiceCallGrant> {
        self.ensure_active_record_enabled(&request.caller_extension_id, "caller")?;
        self.ensure_active_record_enabled(&request.callee_extension_id, "callee")?;

        let caller_manifest = self
            .active_manifest(&request.caller_extension_id)
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: request.caller_extension_id.clone(),
            })?;
        let callee_manifest = self
            .active_manifest(&request.callee_extension_id)
            .ok_or_else(|| RegistryError::NotInstalled {
                extension_id: request.callee_extension_id.clone(),
            })?;

        self.ensure_not_blocked(caller_manifest)?;
        self.ensure_not_blocked(callee_manifest)?;

        let consume = caller_manifest
            .services
            .consumes
            .iter()
            .find(|dependency| {
                dependency.extension_id == request.callee_extension_id
                    && dependency.service == request.service
                    && dependency.method == request.method
                    && dependency.schema_version == request.schema_version
            })
            .ok_or_else(|| RegistryError::ServiceDenied {
                reason: "caller did not declare services.consumes for this callee service"
                    .to_string(),
            })?;

        let provide = callee_manifest
            .services
            .provides
            .iter()
            .find(|definition| {
                definition.service == request.service
                    && definition.method == request.method
                    && definition.schema_version == request.schema_version
            })
            .ok_or_else(|| RegistryError::ServiceUnavailable {
                reason: "callee did not declare services.provides for this service".to_string(),
            })?;

        let required_permission = request
            .required_permission
            .as_deref()
            .unwrap_or(&provide.required_permission);

        if consume.required_permission != required_permission
            || provide.required_permission != required_permission
        {
            return Err(RegistryError::ServiceDenied {
                reason: "caller consume permission, callee provide permission, and request permission must match"
                    .to_string(),
            });
        }

        if !caller_manifest
            .permissions
            .contains_key(required_permission)
        {
            return Err(RegistryError::ServiceDenied {
                reason: format!("caller does not have approved permission {required_permission}"),
            });
        }

        let provider_permission = callee_manifest
            .permissions
            .get(required_permission)
            .ok_or_else(|| RegistryError::ServiceDenied {
                reason: format!("callee permission metadata missing for {required_permission}"),
            })?;
        let caller_permission = caller_manifest
            .permissions
            .get(required_permission)
            .ok_or_else(|| RegistryError::ServiceDenied {
                reason: format!("caller does not have approved permission {required_permission}"),
            })?;
        if caller_permission != provider_permission {
            return Err(RegistryError::ServiceDenied {
                reason: "caller permission metadata does not match provider permission metadata"
                    .to_string(),
            });
        }

        let caller_version = self
            .active_versions
            .get(&request.caller_extension_id)
            .cloned()
            .unwrap_or_default();
        let callee_version = self
            .active_versions
            .get(&request.callee_extension_id)
            .cloned()
            .unwrap_or_default();

        Ok(ServiceCallGrant {
            caller_extension_id: request.caller_extension_id,
            caller_version,
            callee_extension_id: request.callee_extension_id,
            callee_version,
            service: request.service,
            method: request.method,
            schema_version: request.schema_version,
            required_permission: required_permission.to_string(),
        })
    }

    fn ensure_active_record_enabled(&self, extension_id: &str, role: &str) -> RegistryResult<()> {
        let record =
            self.active_record(extension_id)
                .ok_or_else(|| RegistryError::NotInstalled {
                    extension_id: extension_id.to_string(),
                })?;

        match record.status {
            ExtensionInstallStatus::Installed
            | ExtensionInstallStatus::InstalledPreviousVersion => Ok(()),
            ExtensionInstallStatus::Disabled => Err(RegistryError::ServiceDenied {
                reason: format!("{role} extension {extension_id} is disabled"),
            }),
            ExtensionInstallStatus::Blocked => Err(RegistryError::ServiceDenied {
                reason: format!("{role} extension {extension_id} is blocked"),
            }),
            ExtensionInstallStatus::Updating | ExtensionInstallStatus::RollbackInProgress => {
                Err(RegistryError::ServiceDenied {
                    reason: format!(
                        "{role} extension {extension_id} is not ready for service calls"
                    ),
                })
            }
        }
    }

    fn active_manifest(&self, extension_id: &str) -> Option<&ExtensionManifest> {
        let version = self.active_versions.get(extension_id)?;
        self.manifests.get(extension_id)?.get(version)
    }

    fn find_blocklist_hit(&self, manifest: &ExtensionManifest) -> Option<&BlocklistEntry> {
        self.blocklist
            .iter()
            .find(|entry| entry.matches_manifest(manifest))
    }

    fn ensure_same_version_digest_is_stable(
        &self,
        manifest: &ExtensionManifest,
    ) -> RegistryResult<()> {
        let Some(existing) = self
            .records
            .get(&manifest.id)
            .and_then(|records| records.get(&manifest.version))
        else {
            return Ok(());
        };

        if existing.manifest_digest != manifest.provenance.manifest_digest
            || existing.artifact_digest != manifest.provenance.artifact_digest
        {
            return Err(RegistryError::DigestConflict {
                extension_id: manifest.id.clone(),
                version: manifest.version.clone(),
            });
        }

        Ok(())
    }

    fn ensure_source_change_is_approved(
        &self,
        manifest: &ExtensionManifest,
        options: InstallOptions,
    ) -> RegistryResult<()> {
        let Some(current) = self.active_record(&manifest.id) else {
            return Ok(());
        };

        let next = ExtensionSourceSnapshot::from_provenance(&manifest.provenance);
        if current.source.matches_authority(&next) || options.approve_source_change == Some(true) {
            return Ok(());
        }

        Err(RegistryError::SourceChangeRequiresApproval {
            extension_id: manifest.id.clone(),
        })
    }

    fn ensure_not_blocked(&self, manifest: &ExtensionManifest) -> RegistryResult<()> {
        if let Some(entry) = self.find_blocklist_hit(manifest) {
            return Err(RegistryError::Blocked {
                extension_id: manifest.id.clone(),
                reason: entry.reason,
                key: entry.key.clone(),
            });
        }

        Ok(())
    }
}

fn validate_extension_registry_snapshot(
    snapshot: &ExtensionRegistrySnapshot,
) -> Result<(), String> {
    for (extension_id, records) in &snapshot.records {
        for (version, record) in records {
            let manifests = snapshot.manifests.get(extension_id).ok_or_else(|| {
                format!("missing manifests map for extension {extension_id} in snapshot")
            })?;
            let manifest = manifests.get(version).ok_or_else(|| {
                format!("active manifest missing for extension {extension_id} version {version}")
            })?;

            if record.id != *extension_id {
                return Err(format!(
                    "record id {} does not match extension map key {}",
                    record.id, extension_id
                ));
            }
            if record.version != *version {
                return Err(format!(
                    "record version {} does not match extension map key {}",
                    record.version, version
                ));
            }
            if let Some(previous_version) = &record.previous_version {
                if previous_version == version {
                    return Err(format!(
                        "record {extension_id}:{version} has self-referential previous_version"
                    ));
                }
                if !manifests.contains_key(previous_version)
                    || !records.contains_key(previous_version)
                {
                    return Err(format!(
                        "record {extension_id}:{version} references missing previous_version {previous_version}"
                    ));
                }
            }
            if manifest.id != *extension_id {
                return Err(format!(
                    "manifest id {} does not match extension map key {}",
                    manifest.id, extension_id
                ));
            }
            if manifest.version != *version {
                return Err(format!(
                    "manifest version {} does not match extension map key {}",
                    manifest.version, version
                ));
            }
            if manifest.provenance.artifact_digest != record.artifact_digest {
                return Err(format!(
                    "artifact digest mismatch for extension {extension_id} version {version}"
                ));
            }
            if manifest.provenance.manifest_digest != record.manifest_digest {
                return Err(format!(
                    "manifest digest mismatch for extension {extension_id} version {version}"
                ));
            }
            if record.source != ExtensionSourceSnapshot::from_provenance(&manifest.provenance) {
                return Err(format!(
                    "source snapshot mismatch for extension {extension_id} version {version}"
                ));
            }
        }
    }

    for (extension_id, manifests) in &snapshot.manifests {
        for (version, manifest) in manifests {
            let records = snapshot.records.get(extension_id).ok_or_else(|| {
                format!("missing records map for extension {extension_id} in snapshot")
            })?;
            let record = records.get(version).ok_or_else(|| {
                format!("missing record for extension {extension_id} version {version}")
            })?;

            if manifest.id != *extension_id || manifest.version != *version {
                return Err(format!(
                    "manifest key {extension_id}:{version} does not match manifest body {}:{}",
                    manifest.id, manifest.version
                ));
            }
            if manifest.provenance.artifact_digest != record.artifact_digest {
                return Err(format!(
                    "record artifact digest mismatch for extension {extension_id} version {version}"
                ));
            }
            if manifest.provenance.manifest_digest != record.manifest_digest {
                return Err(format!(
                    "record manifest digest mismatch for extension {extension_id} version {version}"
                ));
            }
        }
    }

    for (extension_id, active_version) in &snapshot.active_versions {
        let records = snapshot
            .records
            .get(extension_id)
            .ok_or_else(|| format!("active extension {extension_id} has no record map"))?;
        records.get(active_version).ok_or_else(|| {
            format!("active extension {extension_id} references unknown version {active_version}")
        })?;

        let manifests = snapshot
            .manifests
            .get(extension_id)
            .ok_or_else(|| format!("active extension {extension_id} has no manifest map"))?;
        manifests.get(active_version).ok_or_else(|| {
            format!(
                "active extension {extension_id} references unknown manifest version {active_version}"
            )
        })?;
    }

    if snapshot
        .blocklist
        .iter()
        .any(|entry| matches!(entry.key, BlockKey::VulnerabilityId(_)))
    {
        return Err("extension blocklist contains unsupported vulnerability_id key".to_string());
    }

    Ok(())
}

fn validate_extension_registry_snapshot_manifests(
    snapshot: &ExtensionRegistrySnapshot,
    policy: &ManifestValidationPolicy,
) -> Result<(), String> {
    for (extension_id, records) in &snapshot.records {
        for (version, record) in records {
            let manifest = snapshot
                .manifests
                .get(extension_id)
                .and_then(|manifests| manifests.get(version))
                .ok_or_else(|| {
                    format!("manifest missing for extension {extension_id} version {version}")
                })?;
            let validated = manifest.validate(policy).map_err(|error| {
                format!("manifest validation failed for extension {extension_id} version {version}: {error}")
            })?;
            if validated.boundary != record.boundary {
                return Err(format!(
                    "boundary mismatch for extension {extension_id} version {version}: manifest validates as {:?}, record stores {:?}",
                    validated.boundary, record.boundary
                ));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionBlocklistMatch {
    pub reason: BlockReason,
    pub key: BlockKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionStatusSnapshot {
    pub extension_id: String,
    pub record: ExtensionInstallRecord,
    pub block: Option<ExtensionBlocklistMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallExtensionRequest {
    pub manifest: ExtensionManifest,
    #[serde(default)]
    pub approve_local_unsigned: bool,
    #[serde(default)]
    pub allow_local_process_runtime: bool,
    /// Explicitly approve a source authority change for an install or update.
    #[serde(default)]
    pub approve_source_change: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Request body for side-effect-free manifest validation.
pub struct ValidateExtensionManifestRequest {
    /// Manifest to validate.
    pub manifest: ExtensionManifest,
    /// Allow unsigned local-development manifests for this validation.
    #[serde(default)]
    pub approve_local_unsigned: bool,
    /// Allow local process runtimes for this validation.
    #[serde(default)]
    pub allow_local_process_runtime: bool,
    /// Explicitly approve a source authority change for this validation.
    #[serde(default)]
    pub approve_source_change: bool,
}

impl ValidateExtensionManifestRequest {
    /// Build a validation request using the same defaults as install requests.
    pub fn with_defaults(manifest: ExtensionManifest) -> Self {
        Self {
            manifest,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
        }
    }
}

impl From<ValidateExtensionManifestRequest> for InstallOptions {
    /// Convert an owned validation request into registry install-policy options.
    fn from(request: ValidateExtensionManifestRequest) -> Self {
        Self::from(&request)
    }
}

impl From<&ValidateExtensionManifestRequest> for InstallOptions {
    /// Convert a borrowed validation request into registry install-policy options.
    fn from(request: &ValidateExtensionManifestRequest) -> Self {
        let mut options = InstallOptions::default();
        if request.approve_local_unsigned {
            options = options.approve_local_unsigned();
        }
        if request.allow_local_process_runtime {
            options = options.allow_local_process_runtime();
        }
        if request.approve_source_change {
            options = options.approve_source_change();
        }
        options
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Registry response returned after manifest validation succeeds.
pub struct ValidateExtensionManifestResponse {
    /// Manifest after schema validation and normalization.
    pub manifest: ExtensionManifest,
    /// Computed execution boundary for the validated manifest.
    pub boundary: ExtensionBoundary,
}

impl InstallExtensionRequest {
    pub fn with_defaults(manifest: ExtensionManifest) -> Self {
        Self {
            manifest,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
        }
    }
}

impl From<InstallExtensionRequest> for InstallOptions {
    fn from(request: InstallExtensionRequest) -> Self {
        Self::from(&request)
    }
}

impl From<&InstallExtensionRequest> for InstallOptions {
    fn from(request: &InstallExtensionRequest) -> Self {
        let mut options = InstallOptions::default();
        if request.approve_local_unsigned {
            options = options.approve_local_unsigned();
        }
        if request.allow_local_process_runtime {
            options = options.allow_local_process_runtime();
        }
        if request.approve_source_change {
            options = options.approve_source_change();
        }
        options
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateExtensionRequest {
    pub manifest: ExtensionManifest,
    #[serde(default)]
    pub approve_local_unsigned: bool,
    #[serde(default)]
    pub allow_local_process_runtime: bool,
    /// Explicitly approve a source authority change for this update.
    #[serde(default)]
    pub approve_source_change: bool,
}

impl From<UpdateExtensionRequest> for InstallOptions {
    fn from(request: UpdateExtensionRequest) -> Self {
        Self::from(&request)
    }
}

impl From<&UpdateExtensionRequest> for InstallOptions {
    fn from(request: &UpdateExtensionRequest) -> Self {
        let mut options = InstallOptions::default();
        if request.approve_local_unsigned {
            options = options.approve_local_unsigned();
        }
        if request.allow_local_process_runtime {
            options = options.allow_local_process_runtime();
        }
        if request.approve_source_change {
            options = options.approve_source_change();
        }
        options
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionStatusRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionStatusResponse {
    pub extension_id: String,
    pub record: Option<ExtensionInstallRecord>,
    pub block: Option<ExtensionBlocklistMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListExtensionsRequest {
    #[serde(default = "ListExtensionsRequest::default_include_blocked")]
    pub include_blocked: bool,
}

impl ListExtensionsRequest {
    fn default_include_blocked() -> bool {
        true
    }
}

impl Default for ListExtensionsRequest {
    fn default() -> Self {
        Self {
            include_blocked: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListExtensionsResponse {
    pub extensions: Vec<ExtensionStatusResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackExtensionRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisableExtensionRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisableExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnableExtensionRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnableExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveExtensionRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveExtensionResponse {
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyBlocklistRequest {
    pub entry: BlocklistEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyBlocklistResponse {
    pub entry: BlocklistEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListBlocklistRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListBlocklistResponse {
    pub entries: Vec<BlocklistEntry>,
}

pub struct ExtensionRegistryService {
    registry: ExtensionRegistry,
}

impl ExtensionRegistryService {
    pub fn new() -> Self {
        Self {
            registry: ExtensionRegistry::in_memory(),
        }
    }

    pub fn with_registry(registry: ExtensionRegistry) -> Self {
        Self { registry }
    }

    pub fn snapshot(&self) -> ExtensionRegistrySnapshot {
        self.registry.snapshot()
    }

    pub fn validation_policy(&self) -> ManifestValidationPolicy {
        self.registry.validation_policy()
    }

    pub fn install_extension(
        &mut self,
        request: InstallExtensionRequest,
    ) -> RegistryResult<InstallExtensionResponse> {
        let options = InstallOptions::from(&request);
        let record = self
            .registry
            .install(request.manifest, options)
            .map(|record| InstallExtensionResponse { record })?;
        Ok(record)
    }

    /// Validate a manifest against registry policy without installing it.
    pub fn validate_extension_manifest(
        &self,
        request: ValidateExtensionManifestRequest,
    ) -> RegistryResult<ValidateExtensionManifestResponse> {
        let options = InstallOptions::from(&request);
        self.registry.validate(request.manifest, options)
    }

    pub fn update_extension(
        &mut self,
        request: UpdateExtensionRequest,
    ) -> RegistryResult<UpdateExtensionResponse> {
        let options = InstallOptions::from(&request);
        let record = self
            .registry
            .install(request.manifest, options)
            .map(|record| UpdateExtensionResponse { record })?;
        Ok(record)
    }

    pub fn extension_status(
        &self,
        request: ExtensionStatusRequest,
    ) -> RegistryResult<ExtensionStatusResponse> {
        self.registry
            .extension_status(&request.extension_id)
            .map(|status| status.into())
    }

    pub fn list_extensions(
        &self,
        request: ListExtensionsRequest,
    ) -> RegistryResult<ListExtensionsResponse> {
        let mut extensions: Vec<ExtensionStatusResponse> = self
            .registry
            .list_extension_statuses()?
            .into_iter()
            .map(ExtensionStatusSnapshot::into)
            .collect();

        if !request.include_blocked {
            extensions.retain(|snapshot| snapshot.block.is_none());
        }

        Ok(ListExtensionsResponse { extensions })
    }

    pub fn rollback_extension(
        &mut self,
        request: RollbackExtensionRequest,
    ) -> RegistryResult<RollbackExtensionResponse> {
        let record = self
            .registry
            .rollback(&request.extension_id)
            .map(|record| RollbackExtensionResponse { record })?;
        Ok(record)
    }

    pub fn disable_extension(
        &mut self,
        request: DisableExtensionRequest,
    ) -> RegistryResult<DisableExtensionResponse> {
        let record = self
            .registry
            .disable(&request.extension_id)
            .map(|record| DisableExtensionResponse { record })?;
        Ok(record)
    }

    pub fn enable_extension(
        &mut self,
        request: EnableExtensionRequest,
    ) -> RegistryResult<EnableExtensionResponse> {
        let record = self
            .registry
            .enable(&request.extension_id)
            .map(|record| EnableExtensionResponse { record })?;
        Ok(record)
    }

    pub fn remove_extension(
        &mut self,
        request: RemoveExtensionRequest,
    ) -> RegistryResult<RemoveExtensionResponse> {
        let record = self
            .registry
            .remove(&request.extension_id)
            .map(|record| RemoveExtensionResponse { record })?;
        Ok(record)
    }

    pub fn apply_blocklist(
        &mut self,
        request: ApplyBlocklistRequest,
    ) -> RegistryResult<ApplyBlocklistResponse> {
        let entry = request.entry;
        self.registry.add_blocklist_entry(entry.clone())?;
        Ok(ApplyBlocklistResponse { entry })
    }

    pub fn list_blocklist(
        &self,
        _request: ListBlocklistRequest,
    ) -> RegistryResult<ListBlocklistResponse> {
        Ok(ListBlocklistResponse {
            entries: self.registry.blocklist_entries(),
        })
    }
}

impl Default for ExtensionRegistryService {
    fn default() -> Self {
        Self::new()
    }
}

impl From<ExtensionStatusSnapshot> for ExtensionStatusResponse {
    fn from(snapshot: ExtensionStatusSnapshot) -> Self {
        Self {
            extension_id: snapshot.extension_id,
            record: Some(snapshot.record),
            block: snapshot.block,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    pub approve_local_unsigned: Option<bool>,
    pub allow_local_process_runtime: Option<bool>,
    pub approve_source_change: Option<bool>,
}

impl InstallOptions {
    pub fn approve_local_unsigned(mut self) -> Self {
        self.approve_local_unsigned = Some(true);
        self
    }

    pub fn allow_local_process_runtime(mut self) -> Self {
        self.allow_local_process_runtime = Some(true);
        self
    }

    /// Allow an install or update to replace the package's recorded source authority.
    pub fn approve_source_change(mut self) -> Self {
        self.approve_source_change = Some(true);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionInstallRecord {
    pub id: String,
    pub version: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
    pub source: ExtensionSourceSnapshot,
    pub boundary: ExtensionBoundary,
    pub status: ExtensionInstallStatus,
    pub previous_version: Option<String>,
    pub approved_permissions: Vec<String>,
    pub rollback_snapshot: Option<RollbackSnapshot>,
}

/// Persisted source provenance snapshot for an installed package revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionSourceSnapshot {
    /// Source class that produced this package revision.
    pub source: ProvenanceSource,
    /// Source repository, when the package is repository-backed.
    pub repository: Option<String>,
    /// Source ref, when the package is repository-backed.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Manifest path inside the source repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    /// Source commit retained for audit, but not treated as authority identity.
    pub commit: Option<String>,
    /// Registry identity, when the package came from a registry.
    pub registry_identity: Option<String>,
    /// Package lineage retained with the installed revision.
    pub lineage: Option<ExtensionLineage>,
    /// Publication state retained with the installed revision.
    pub publication: Option<ExtensionPublication>,
}

impl ExtensionSourceSnapshot {
    /// Build an install-record source snapshot from manifest provenance.
    pub fn from_provenance(provenance: &ExtensionProvenance) -> Self {
        Self {
            source: provenance.source,
            repository: trim_optional_string(provenance.repository.as_deref()),
            source_ref: trim_optional_string(provenance.source_ref.as_deref()),
            manifest_path: trim_optional_string(provenance.manifest_path.as_deref()),
            commit: provenance.commit.clone(),
            registry_identity: trim_optional_string(provenance.registry_identity.as_deref()),
            lineage: provenance.lineage.clone(),
            publication: provenance.publication.clone(),
        }
    }

    fn matches_authority(&self, other: &Self) -> bool {
        self.source == other.source
            && trim_optional_str(self.repository.as_deref())
                == trim_optional_str(other.repository.as_deref())
            && trim_optional_str(self.source_ref.as_deref())
                == trim_optional_str(other.source_ref.as_deref())
            && trim_optional_str(self.manifest_path.as_deref())
                == trim_optional_str(other.manifest_path.as_deref())
            && trim_optional_str(self.registry_identity.as_deref())
                == trim_optional_str(other.registry_identity.as_deref())
            && self.lineage == other.lineage
    }
}

fn trim_optional_string(value: Option<&str>) -> Option<String> {
    trim_optional_str(value).map(ToString::to_string)
}

fn trim_optional_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionInstallStatus {
    Installed,
    Disabled,
    Blocked,
    Updating,
    RollbackInProgress,
    InstalledPreviousVersion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackSnapshot {
    pub manifest_digest: String,
    pub artifact_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCallRequest {
    pub caller_extension_id: String,
    pub callee_extension_id: String,
    pub service: String,
    pub method: String,
    pub schema_version: String,
    pub required_permission: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCallGrant {
    pub caller_extension_id: String,
    pub caller_version: String,
    pub callee_extension_id: String,
    pub callee_version: String,
    pub service: String,
    pub method: String,
    pub schema_version: String,
    pub required_permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlocklistEntry {
    pub key: BlockKey,
    pub reason: BlockReason,
    pub note: Option<String>,
}

impl BlocklistEntry {
    fn matches_manifest(&self, manifest: &ExtensionManifest) -> bool {
        match &self.key {
            BlockKey::ExtensionId(id) => manifest.id == *id,
            BlockKey::Version { id, version } => manifest.id == *id && manifest.version == *version,
            BlockKey::ArtifactDigest(digest) => manifest.provenance.artifact_digest == *digest,
            BlockKey::Signer(signer) => manifest
                .provenance
                .signer
                .as_deref()
                .is_some_and(|manifest_signer| manifest_signer.trim() == signer.trim()),
            BlockKey::Publisher(publisher) => manifest.publisher.name.trim() == publisher.trim(),
            BlockKey::SourceRepository(repository) => manifest
                .provenance
                .repository
                .as_deref()
                .is_some_and(|manifest_repository| manifest_repository.trim() == repository.trim()),
            BlockKey::PermissionPattern(pattern) => manifest
                .permissions
                .keys()
                .any(|permission| permission_matches(pattern, permission)),
            BlockKey::VulnerabilityId(_) => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockKey {
    ExtensionId(String),
    Version { id: String, version: String },
    ArtifactDigest(String),
    Signer(String),
    Publisher(String),
    SourceRepository(String),
    PermissionPattern(String),
    VulnerabilityId(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    Malware,
    ManifestMismatch,
    OverPermissioned,
    VulnerableVersion,
    CompromisedSigner,
    PolicyViolation,
    UserBlocked,
    RegistryRemoved,
}

fn permission_matches(pattern: &str, permission: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        permission.starts_with(prefix)
    } else {
        pattern == permission
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    Validation(ExtensionValidationError),
    UnsupportedBlocklistKey {
        key: BlockKey,
    },
    Blocked {
        extension_id: String,
        reason: BlockReason,
        key: BlockKey,
    },
    DigestConflict {
        extension_id: String,
        version: String,
    },
    SourceChangeRequiresApproval {
        extension_id: String,
    },
    NotInstalled {
        extension_id: String,
    },
    RollbackUnavailable {
        extension_id: String,
    },
    ServiceDenied {
        reason: String,
    },
    ServiceUnavailable {
        reason: String,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(f, "{error}"),
            Self::UnsupportedBlocklistKey { key } => {
                write!(f, "unsupported blocklist key: {key:?}")
            }
            Self::Blocked {
                extension_id,
                reason,
                key,
            } => write!(
                f,
                "extension {extension_id} is blocklisted by {reason:?}: {key:?}"
            ),
            Self::DigestConflict {
                extension_id,
                version,
            } => write!(
                f,
                "extension {extension_id} version {version} changed digest"
            ),
            Self::SourceChangeRequiresApproval { extension_id } => write!(
                f,
                "extension {extension_id} changed source provenance and requires explicit approval"
            ),
            Self::NotInstalled { extension_id } => {
                write!(f, "extension {extension_id} is not installed")
            }
            Self::RollbackUnavailable { extension_id } => {
                write!(f, "extension {extension_id} has no rollback target")
            }
            Self::ServiceDenied { reason } => write!(f, "service call denied: {reason}"),
            Self::ServiceUnavailable { reason } => write!(f, "service unavailable: {reason}"),
        }
    }
}

impl Error for RegistryError {}

impl From<ExtensionValidationError> for RegistryError {
    fn from(error: ExtensionValidationError) -> Self {
        Self::Validation(error)
    }
}

pub type RegistryResult<T> = Result<T, RegistryError>;

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIFACT_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MANIFEST_DIGEST: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OTHER_ARTIFACT_DIGEST: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const OTHER_MANIFEST_DIGEST: &str =
        "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const THIRD_ARTIFACT_DIGEST: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const THIRD_MANIFEST_DIGEST: &str =
        "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    const DEFAULT_TOOL_PERMISSION: &str = "tool.ping";

    fn permission(description: &str) -> ExtensionPermission {
        ExtensionPermission {
            description: description.to_string(),
            risk_tier: Some("R2".to_string()),
        }
    }

    fn manifest(id: &str) -> ExtensionManifest {
        let mut permissions = BTreeMap::new();
        permissions.insert(
            DEFAULT_TOOL_PERMISSION.to_string(),
            permission("exposes the ping tool"),
        );

        ExtensionManifest {
            schema: EXTENSION_MANIFEST_SCHEMA.to_string(),
            id: id.to_string(),
            name: "Test Extension".to_string(),
            version: "1.0.0".to_string(),
            publisher: ExtensionPublisher {
                name: "Example Publisher".to_string(),
                url: Some("https://example.com".to_string()),
            },
            description: "A focused test extension".to_string(),
            types: vec![ExtensionKind::Tool],
            compatibility: ExtensionCompatibility {
                atelia_protocol: ">=0.1 <0.3".to_string(),
                atelia_secretary: ">=0.1 <0.2".to_string(),
            },
            entrypoints: ExtensionEntrypoints {
                realm: ExtensionRealm::Backend,
                runtime: ExtensionRuntime::WasmRust,
                command: None,
                image: None,
                wasm: Some("extension.wasm".to_string()),
                protocol: EXTENSION_RPC_PROTOCOL.to_string(),
            },
            permissions,
            tools: vec![ExtensionToolDefinition {
                id: "ping".to_string(),
                permissions: vec![DEFAULT_TOOL_PERMISSION.to_string()],
                permissions_required: Vec::new(),
            }],
            services: ExtensionServices::default(),
            failure: ExtensionFailure {
                degrade: DegradeBehavior::ReturnUnavailable,
                retry_policy: RetryPolicy::Bounded,
            },
            provenance: ExtensionProvenance {
                source: ProvenanceSource::Registry,
                repository: None,
                source_ref: None,
                manifest_path: None,
                commit: None,
                registry_identity: Some("third-party-registry".to_string()),
                lineage: None,
                publication: None,
                artifact_digest: ARTIFACT_DIGEST.to_string(),
                manifest_digest: MANIFEST_DIGEST.to_string(),
                signature: Some("signature".to_string()),
                signer: Some("signer@example.com".to_string()),
            },
            ..ExtensionManifest::default()
        }
    }

    fn github_manifest(id: &str) -> ExtensionManifest {
        let mut manifest = manifest(id);
        manifest.provenance.source = ProvenanceSource::Github;
        manifest.provenance.repository = Some("https://github.com/example/package".to_string());
        manifest.provenance.source_ref = Some("refs/heads/main".to_string());
        manifest.provenance.manifest_path = Some("atelia.package.yaml".to_string());
        manifest.provenance.commit = Some("1111111".to_string());
        manifest.provenance.registry_identity = None;
        manifest
    }

    fn service_provider(id: &str, permission_name: &str) -> ExtensionManifest {
        let mut manifest = manifest(id);
        manifest.types = vec![ExtensionKind::Service];
        manifest.tools.clear();
        manifest
            .permissions
            .insert(permission_name.to_string(), permission("provide service"));
        manifest.services.provides.push(ExtensionServiceDefinition {
            service: "review.comments".to_string(),
            method: "summarize".to_string(),
            schema_version: "v1".to_string(),
            required_permission: permission_name.to_string(),
        });
        manifest
    }

    fn service_consumer(id: &str, callee_id: &str, permission_name: &str) -> ExtensionManifest {
        let mut manifest = manifest(id);
        manifest.types = vec![ExtensionKind::Service];
        manifest.tools.clear();
        manifest.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();
        manifest.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        manifest
            .permissions
            .insert(permission_name.to_string(), permission("provide service"));
        manifest.services.consumes.push(ExtensionServiceDependency {
            extension_id: callee_id.to_string(),
            service: "review.comments".to_string(),
            method: "summarize".to_string(),
            schema_version: "v1".to_string(),
            required_permission: permission_name.to_string(),
        });
        manifest
    }

    fn service_call(caller_id: &str, callee_id: &str) -> ServiceCallRequest {
        ServiceCallRequest {
            caller_extension_id: caller_id.to_string(),
            callee_extension_id: callee_id.to_string(),
            service: "review.comments".to_string(),
            method: "summarize".to_string(),
            schema_version: "v1".to_string(),
            required_permission: Some("service.review.comments".to_string()),
        }
    }

    fn tool_output_definition(tool_id: &str) -> ExtensionToolOutputDefinition {
        ExtensionToolOutputDefinition {
            tool_id: tool_id.to_string(),
            format: Some("toon".to_string()),
            verbosity: Some("normal".to_string()),
            language_mode: Some("english_agent".to_string()),
            fields: vec!["summary".to_string()],
            redactions: vec!["secret".to_string()],
            include_policy: Some(true),
            include_cost: Some(false),
            include_diagnostics: Some(true),
        }
    }

    fn hook_definition(hook_id: &str) -> ExtensionHookDefinition {
        ExtensionHookDefinition {
            hook_id: hook_id.to_string(),
            trigger: Some("pull_request.opened".to_string()),
            verification: Some("github_signature".to_string()),
            required_capabilities: vec!["review.comment".to_string()],
            action: Some("workflow".to_string()),
            status: Some("enabled".to_string()),
        }
    }

    fn webhook_definition(webhook_id: &str) -> ExtensionWebhookDefinition {
        ExtensionWebhookDefinition {
            webhook_id: webhook_id.to_string(),
            source: Some("github".to_string()),
            event: Some("pull_request.opened".to_string()),
            endpoint: Some("https://example.com/webhook".to_string()),
            verification: Some("hmac".to_string()),
            required_capabilities: vec!["network.webhook.receive:github".to_string()],
            status: Some("enabled".to_string()),
        }
    }

    fn extension_record(
        manifest: &ExtensionManifest,
        previous_version: Option<&str>,
    ) -> ExtensionInstallRecord {
        ExtensionInstallRecord {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            manifest_digest: manifest.provenance.manifest_digest.clone(),
            artifact_digest: manifest.provenance.artifact_digest.clone(),
            source: ExtensionSourceSnapshot::from_provenance(&manifest.provenance),
            boundary: ExtensionBoundary::ThirdParty,
            status: ExtensionInstallStatus::Installed,
            previous_version: previous_version.map(str::to_string),
            approved_permissions: Vec::new(),
            rollback_snapshot: None,
        }
    }

    fn extension_snapshot(
        manifest_versions: BTreeMap<String, ExtensionManifest>,
        record_versions: BTreeMap<String, ExtensionInstallRecord>,
    ) -> ExtensionRegistrySnapshot {
        let mut manifests = BTreeMap::new();
        manifests.insert("com.example.extension".to_string(), manifest_versions);

        let mut records = BTreeMap::new();
        records.insert("com.example.extension".to_string(), record_versions);

        ExtensionRegistrySnapshot {
            manifests,
            records,
            active_versions: BTreeMap::new(),
            blocklist: Vec::new(),
        }
    }

    #[test]
    fn validates_backend_wasm_rust_manifest() {
        let validated = manifest("com.example.extension")
            .validate(&ManifestValidationPolicy::default())
            .unwrap();

        assert_eq!(validated.boundary, ExtensionBoundary::ThirdParty);
    }

    #[test]
    fn tool_type_requires_tool_declarations() {
        let mut extension = manifest("com.example.tool");
        extension.tools.clear();

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::MissingField { field: "tools" }
        ));
    }

    #[test]
    fn tools_require_declared_extension_permissions() {
        let mut extension = manifest("com.example.tool-permission");
        extension.permissions.clear();

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        match err {
            ExtensionValidationError::MissingToolPermission { tool, permission } => {
                assert_eq!(tool, "ping");
                assert_eq!(permission, DEFAULT_TOOL_PERMISSION);
            }
            other => panic!("expected missing tool permission, got {other}"),
        }
    }

    #[test]
    fn tools_validate_permissions_from_both_permissions_fields() {
        let mut extension = manifest("com.example.tool-permission-both");
        extension.tools[0].permissions = vec![DEFAULT_TOOL_PERMISSION.to_string()];
        extension.tools[0].permissions_required = vec!["tool.write".to_string()];

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .expect_err("expected validation failure");

        match err {
            ExtensionValidationError::MissingToolPermission { tool, permission } => {
                assert_eq!(tool, "ping");
                assert_eq!(permission, "tool.write");
            }
            other => panic!("expected missing tool permission, got {other}"),
        }
    }

    #[test]
    fn tools_require_type_tool() {
        let mut extension = manifest("com.example.tool-type-mismatch");
        extension.types = vec![ExtensionKind::Service];

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "types",
                reason: _,
            }
        ));
    }

    #[test]
    fn duplicate_tool_output_declarations_are_rejected() {
        let mut extension = manifest("com.example.duplicate-tool-output");
        extension.types.push(ExtensionKind::ToolOutputCustomizer);
        extension.tool_output.push(tool_output_definition("ping"));
        extension.tool_output.push(tool_output_definition("ping"));

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::DuplicateToolOutputDeclaration { tool_id }
                if tool_id == "ping"
        ));
    }

    #[test]
    fn duplicate_hook_and_webhook_declarations_are_rejected() {
        let mut hooks = manifest("com.example.duplicate-hooks");
        hooks.types.push(ExtensionKind::HookProvider);
        hooks.hooks.push(hook_definition("hk_review"));
        hooks.hooks.push(hook_definition("hk_review"));

        let err = hooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::DuplicateHookDeclaration { hook_id }
                if hook_id == "hk_review"
        ));

        let mut webhooks = manifest("com.example.duplicate-webhooks");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(webhook_definition("wh_review"));
        webhooks.webhooks.push(webhook_definition("wh_review"));

        let err = webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::DuplicateWebhookDeclaration { webhook_id }
                if webhook_id == "wh_review"
        ));
    }

    #[test]
    fn local_only_hook_and_webhook_verification_is_rejected_for_non_local_manifests() {
        let mut hooks = manifest("com.example.hook-verification");
        hooks.types.push(ExtensionKind::HookProvider);
        hooks.hooks.push(ExtensionHookDefinition {
            verification: Some("none_for_local_only".to_string()),
            ..hook_definition("hk_review")
        });

        let err = hooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "hooks.verification",
                ..
            }
        ));

        let mut webhooks = manifest("com.example.webhook-verification");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            verification: Some("none_for_local_only".to_string()),
            ..webhook_definition("wh_review")
        });

        let err = webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.verification",
                ..
            }
        ));
    }

    #[test]
    fn local_only_hook_and_webhook_verification_is_allowed_for_local_manifests() {
        let mut hooks = manifest("local.test.hook-verification");
        hooks.provenance.source = ProvenanceSource::Local;
        hooks.provenance.registry_identity = None;
        hooks.provenance.signature = None;
        hooks.provenance.signer = None;
        hooks.types.push(ExtensionKind::HookProvider);
        hooks.hooks.push(ExtensionHookDefinition {
            verification: Some("none_for_local_only".to_string()),
            ..hook_definition("hk_review")
        });

        hooks
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap();

        let mut webhooks = manifest("local.test.webhook-verification");
        webhooks.provenance.source = ProvenanceSource::Local;
        webhooks.provenance.registry_identity = None;
        webhooks.provenance.signature = None;
        webhooks.provenance.signer = None;
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            verification: Some("none_for_local_only".to_string()),
            ..webhook_definition("wh_review")
        });

        webhooks
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap();
    }

    #[test]
    fn security_sensitive_section_fields_are_validated() {
        let mut extension = manifest("com.example.invalid-tool-output");
        extension.types.push(ExtensionKind::ToolOutputCustomizer);
        extension.tool_output.push(ExtensionToolOutputDefinition {
            tool_id: "ping".to_string(),
            format: Some("markdown".to_string()),
            verbosity: Some("verbose".to_string()),
            language_mode: Some("human".to_string()),
            fields: vec!["summary".to_string()],
            redactions: vec!["secret".to_string()],
            include_policy: Some(true),
            include_cost: Some(false),
            include_diagnostics: Some(true),
        });

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "tool_output.format",
                ..
            }
        ));

        let mut hooks = manifest("com.example.invalid-hook");
        hooks.types.push(ExtensionKind::HookProvider);
        hooks.hooks.push(ExtensionHookDefinition {
            hook_id: "hk_review".to_string(),
            trigger: Some("pull request opened".to_string()),
            verification: Some("oauth".to_string()),
            required_capabilities: vec!["review.comment".to_string()],
            action: Some("workflow".to_string()),
            status: Some("enabled".to_string()),
        });

        let err = hooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "hooks.trigger",
                ..
            }
        ));

        let mut webhooks = manifest("com.example.invalid-webhook");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            webhook_id: "wh_review".to_string(),
            source: Some("slack".to_string()),
            event: Some("pull_request.opened".to_string()),
            endpoint: Some("ftp://example.com/webhook".to_string()),
            verification: Some("hmac".to_string()),
            required_capabilities: vec!["network.webhook.receive:github".to_string()],
            status: Some("enabled".to_string()),
        });

        let err = webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.source",
                ..
            }
        ));
    }

    #[test]
    fn webhook_endpoints_require_https_and_preserve_legacy_omission_compatibility() {
        let mut webhooks = manifest("com.example.webhook-endpoint");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            endpoint: Some("https://example.com/webhook".to_string()),
            ..webhook_definition("wh_review")
        });

        webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap();

        let mut http_webhook = manifest("com.example.webhook-http");
        http_webhook.types.push(ExtensionKind::WebhookReceiver);
        http_webhook.webhooks.push(ExtensionWebhookDefinition {
            endpoint: Some("http://example.com/webhook".to_string()),
            ..webhook_definition("wh_review")
        });

        let err = http_webhook
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.endpoint",
                ..
            }
        ));

        let mut malformed_webhook = manifest("com.example.webhook-malformed");
        malformed_webhook.types.push(ExtensionKind::WebhookReceiver);
        malformed_webhook.webhooks.push(ExtensionWebhookDefinition {
            endpoint: Some("https:/example.com/webhook".to_string()),
            ..webhook_definition("wh_review")
        });

        let err = malformed_webhook
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.endpoint",
                ..
            }
        ));

        let mut missing_endpoint = manifest("com.example.webhook-missing-endpoint");
        missing_endpoint.types.push(ExtensionKind::WebhookReceiver);
        missing_endpoint.webhooks.push(ExtensionWebhookDefinition {
            endpoint: None,
            ..webhook_definition("wh_review")
        });

        let err = missing_endpoint
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::MissingField {
                field: "webhooks.endpoint",
            }
        ));
    }

    #[test]
    fn webhook_endpoints_reject_invalid_bracketed_ipv6_hosts() {
        let mut webhooks = manifest("com.example.webhook-bracketed-host");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            endpoint: Some("https://[foo]/webhook".to_string()),
            ..webhook_definition("wh_review")
        });

        let err = webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.endpoint",
                ..
            }
        ));

        let mut ipv6_webhook = manifest("com.example.webhook-ipv6");
        ipv6_webhook.types.push(ExtensionKind::WebhookReceiver);
        ipv6_webhook.webhooks.push(ExtensionWebhookDefinition {
            endpoint: Some("https://[2001:db8::1]/webhook".to_string()),
            ..webhook_definition("wh_review")
        });

        ipv6_webhook
            .validate(&ManifestValidationPolicy::default())
            .unwrap();
    }

    #[test]
    fn whitespace_padded_duplicate_declaration_ids_are_rejected() {
        let mut tool_output = manifest("com.example.whitespace-tool-output");
        tool_output.types.push(ExtensionKind::ToolOutputCustomizer);
        tool_output.tool_output.push(tool_output_definition("ping"));
        tool_output
            .tool_output
            .push(tool_output_definition(" ping "));

        let err = tool_output
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "tool_output.tool_id",
                ..
            }
        ));

        let mut hooks = manifest("com.example.whitespace-hooks");
        hooks.types.push(ExtensionKind::HookProvider);
        hooks.hooks.push(hook_definition("hk_review"));
        hooks.hooks.push(ExtensionHookDefinition {
            hook_id: " hk_review ".to_string(),
            ..hook_definition("hk_review")
        });

        let err = hooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "hooks.hook_id",
                ..
            }
        ));

        let mut webhooks = manifest("com.example.whitespace-webhooks");
        webhooks.types.push(ExtensionKind::WebhookReceiver);
        webhooks.webhooks.push(webhook_definition("wh_review"));
        webhooks.webhooks.push(ExtensionWebhookDefinition {
            webhook_id: " wh_review ".to_string(),
            ..webhook_definition("wh_review")
        });

        let err = webhooks
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "webhooks.webhook_id",
                ..
            }
        ));
    }

    #[test]
    fn rejects_official_namespace_without_official_registry_identity() {
        let err = manifest("ai.atelia.git")
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();

        assert!(matches!(
            err,
            ExtensionValidationError::BoundaryViolation { .. }
        ));
    }

    #[test]
    fn local_unsigned_requires_dev_mode_approval() {
        let mut local = manifest("local.test.extension");
        local.provenance.source = ProvenanceSource::Local;
        local.provenance.registry_identity = None;
        local.provenance.signature = None;
        local.provenance.signer = None;

        let err = local
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.signature",
                ..
            }
        ));

        let validated = local
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap();
        assert_eq!(validated.boundary, ExtensionBoundary::LocalDevelopment);
    }

    #[test]
    fn whitespace_provenance_fields_are_treated_as_missing() {
        let mut local = manifest("local.test.whitespace");
        local.provenance.source = ProvenanceSource::Local;
        local.provenance.signature = Some("   ".to_string());
        local.provenance.signer = Some("\n\t".to_string());

        let err = local
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.signature",
                ..
            }
        ));

        let mut github = manifest("com.example.github");
        github.provenance.source = ProvenanceSource::Github;
        github.provenance.repository = Some("   ".to_string());
        let err = github
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.repository",
                ..
            }
        ));

        let mut registry = manifest("com.example.registry");
        registry.provenance.source = ProvenanceSource::Registry;
        registry.provenance.registry_identity = Some(" \t ".to_string());
        let err = registry
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.registry_identity",
                ..
            }
        ));
    }

    #[test]
    fn github_source_reference_fields_are_required() {
        let mut missing_ref = github_manifest("com.example.github");
        missing_ref.provenance.source_ref = None;
        let err = missing_ref
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.ref",
                ..
            }
        ));

        let mut missing_manifest_path = github_manifest("com.example.github");
        missing_manifest_path.provenance.manifest_path = Some("   ".to_string());
        let err = missing_manifest_path
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::ProvenanceRequired {
                field: "provenance.manifest_path",
                ..
            }
        ));
    }

    #[test]
    fn backend_process_runtime_is_local_development_only() {
        let mut process = manifest("local.test.process");
        process.provenance.source = ProvenanceSource::Local;
        process.provenance.registry_identity = None;
        process.entrypoints.runtime = ExtensionRuntime::Process;
        process.entrypoints.wasm = None;
        process.entrypoints.command = Some("cargo run".to_string());

        let err = process
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::UnsupportedRuntime {
                runtime: ExtensionRuntime::Process,
                ..
            }
        ));

        process
            .validate(
                &ManifestValidationPolicy::default()
                    .with_local_unsigned()
                    .with_local_process_runtime(),
            )
            .unwrap();
    }

    #[test]
    fn service_dependencies_must_declare_permissions() {
        let mut consumer = service_consumer(
            "com.example.consumer",
            "com.example.provider",
            "service.review.comments",
        );
        consumer.permissions.clear();

        let err = consumer
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::MissingServicePermission { .. }
        ));
    }

    #[test]
    fn registry_blocks_install_before_local_enablement() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .add_blocklist_entry(BlocklistEntry {
                key: BlockKey::ExtensionId("com.example.extension".to_string()),
                reason: BlockReason::UserBlocked,
                note: None,
            })
            .unwrap();

        let err = registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap_err();

        assert!(matches!(
            err,
            RegistryError::Blocked {
                reason: BlockReason::UserBlocked,
                ..
            }
        ));
    }

    #[test]
    fn install_options_default_preserves_validation_policy() {
        let mut registry = ExtensionRegistry::new(ManifestValidationPolicy {
            allow_local_unsigned: true,
            allow_local_process_runtime: true,
            ..ManifestValidationPolicy::default()
        });

        let mut unsigned = manifest("local.test.unsigned");
        unsigned.provenance.source = ProvenanceSource::Local;
        unsigned.provenance.registry_identity = None;
        unsigned.provenance.signature = None;
        unsigned.provenance.signer = None;

        registry
            .install(unsigned, InstallOptions::default())
            .unwrap();

        let mut process = manifest("local.test.process");
        process.provenance.source = ProvenanceSource::Local;
        process.provenance.registry_identity = None;
        process.entrypoints.runtime = ExtensionRuntime::Process;
        process.entrypoints.wasm = None;
        process.entrypoints.command = Some("cargo run".to_string());

        registry
            .install(process, InstallOptions::default())
            .unwrap();
    }

    #[test]
    /// Validating a candidate manifest succeeds without mutating registry maps.
    fn validate_extension_manifest_passes_without_mutating_registry_state() {
        let mut registry = ExtensionRegistry::in_memory();
        let mut manifest_v1 = manifest("com.example.extension");
        manifest_v1.version = "1.0.0".to_string();
        registry
            .install(manifest_v1.clone(), InstallOptions::default())
            .unwrap();

        let manifests_before = registry.manifests.clone();
        let records_before = registry.records.clone();
        let active_versions_before = registry.active_versions.clone();
        let blocklist_before = registry.blocklist.clone();

        let mut manifest_v2 = manifest("com.example.extension");
        manifest_v2.version = "1.1.0".to_string();
        manifest_v2.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        manifest_v2.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        let validated = registry
            .validate(manifest_v2.clone(), InstallOptions::default())
            .unwrap();
        assert_eq!(validated.manifest.version, manifest_v2.version);
        assert_eq!(validated.boundary, ExtensionBoundary::ThirdParty);

        assert_eq!(registry.manifests, manifests_before);
        assert_eq!(registry.records, records_before);
        assert_eq!(registry.active_versions, active_versions_before);
        assert_eq!(registry.blocklist, blocklist_before);
    }

    #[test]
    /// Invalid manifest validation fails without mutating registry maps.
    fn validate_extension_manifest_rejects_invalid_manifest_without_mutating_registry_state() {
        let mut registry = ExtensionRegistry::in_memory();
        let mut manifest_v1 = manifest("com.example.extension");
        manifest_v1.version = "1.0.0".to_string();
        registry
            .install(manifest_v1.clone(), InstallOptions::default())
            .unwrap();

        let manifests_before = registry.manifests.clone();
        let records_before = registry.records.clone();
        let active_versions_before = registry.active_versions.clone();
        let blocklist_before = registry.blocklist.clone();

        let mut manifest_v2 = manifest("com.example.extension");
        manifest_v2.version = "not-semver".to_string();

        let err = registry
            .validate(manifest_v2, InstallOptions::default())
            .unwrap_err();

        assert!(matches!(
            err,
            RegistryError::Validation(ExtensionValidationError::InvalidField {
                field: "version",
                ..
            })
        ));

        assert_eq!(registry.manifests, manifests_before);
        assert_eq!(registry.records, records_before);
        assert_eq!(registry.active_versions, active_versions_before);
        assert_eq!(registry.blocklist, blocklist_before);
    }

    #[test]
    fn blocklist_matches_trimmed_signer() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .add_blocklist_entry(BlocklistEntry {
                key: BlockKey::Signer(" signer@example.com ".to_string()),
                reason: BlockReason::UserBlocked,
                note: None,
            })
            .unwrap();

        let mut blocked = manifest("com.example.extension");
        blocked.provenance.signer = Some(" signer@example.com ".to_string());

        let err = registry
            .install(blocked, InstallOptions::default())
            .unwrap_err();

        assert!(matches!(err, RegistryError::Blocked { .. }));
    }

    #[test]
    fn blocklist_matches_trimmed_publisher() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .add_blocklist_entry(BlocklistEntry {
                key: BlockKey::Publisher(" Example Publisher ".to_string()),
                reason: BlockReason::UserBlocked,
                note: None,
            })
            .unwrap();

        let mut blocked = manifest("com.example.extension");
        blocked.publisher.name = " Example Publisher ".to_string();

        let err = registry
            .install(blocked, InstallOptions::default())
            .unwrap_err();

        assert!(matches!(err, RegistryError::Blocked { .. }));
    }

    #[test]
    fn blocklist_matches_trimmed_source_repository() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .add_blocklist_entry(BlocklistEntry {
                key: BlockKey::SourceRepository("https://example.com/repo ".to_string()),
                reason: BlockReason::UserBlocked,
                note: None,
            })
            .unwrap();

        let mut blocked = manifest("com.example.extension");
        blocked.provenance.source = ProvenanceSource::Github;
        blocked.provenance.registry_identity = Some("github-registry".to_string());
        blocked.provenance.repository = Some(" https://example.com/repo ".to_string());
        blocked.provenance.source_ref = Some("refs/heads/main".to_string());
        blocked.provenance.manifest_path = Some("atelia.package.yaml".to_string());
        blocked.provenance.commit = Some("abc123".to_string());

        let err = registry
            .install(blocked, InstallOptions::default())
            .unwrap_err();

        assert!(matches!(err, RegistryError::Blocked { .. }));
    }

    #[test]
    fn unsupported_vulnerability_block_key_is_rejected() {
        let mut registry = ExtensionRegistry::in_memory();
        let err = registry.add_blocklist_entry(BlocklistEntry {
            key: BlockKey::VulnerabilityId("CVE-0000-0000".to_string()),
            reason: BlockReason::VulnerableVersion,
            note: None,
        });

        assert!(matches!(
            err,
            Err(RegistryError::UnsupportedBlocklistKey { .. })
        ));
    }

    #[test]
    fn rollback_fails_without_state_change_when_previous_version_is_blocked() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let mut next = manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        registry.install(next, InstallOptions::default()).unwrap();

        registry
            .add_blocklist_entry(BlocklistEntry {
                key: BlockKey::ArtifactDigest(ARTIFACT_DIGEST.to_string()),
                reason: BlockReason::VulnerableVersion,
                note: None,
            })
            .unwrap();

        let err = registry.rollback("com.example.extension").unwrap_err();
        assert!(matches!(err, RegistryError::Blocked { .. }));
        assert_eq!(
            registry
                .active_record("com.example.extension")
                .unwrap()
                .version,
            "1.1.0"
        );
        assert_eq!(
            registry
                .active_record("com.example.extension")
                .unwrap()
                .status,
            ExtensionInstallStatus::Installed
        );
    }

    #[test]
    fn registry_enable_disable_and_remove_manage_active_record() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let disabled = registry.disable("com.example.extension").unwrap();
        assert_eq!(disabled.status, ExtensionInstallStatus::Disabled);
        assert_eq!(
            registry
                .active_record("com.example.extension")
                .unwrap()
                .status,
            ExtensionInstallStatus::Disabled
        );

        let enabled = registry.enable("com.example.extension").unwrap();
        assert_eq!(enabled.status, ExtensionInstallStatus::Installed);

        let removed = registry.remove("com.example.extension").unwrap();
        assert_eq!(removed.status, ExtensionInstallStatus::Disabled);
        assert!(registry.active_record("com.example.extension").is_none());
        assert!(matches!(
            registry.disable("com.example.extension"),
            Err(RegistryError::NotInstalled { .. })
        ));
    }

    #[test]
    fn same_version_different_digest_is_rejected() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let mut changed = manifest("com.example.extension");
        changed.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();

        let err = registry
            .install(changed, InstallOptions::default())
            .unwrap_err();

        assert!(matches!(
            err,
            RegistryError::DigestConflict {
                extension_id,
                version
            } if extension_id == "com.example.extension" && version == "1.0.0"
        ));
    }

    #[test]
    fn same_version_reinstall_does_not_create_self_referential_rollback() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let reinstalled = registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        assert_eq!(reinstalled.version, "1.0.0");
        assert_eq!(reinstalled.previous_version, None);
        registry.snapshot().validate().unwrap();
    }

    #[test]
    fn same_bundle_service_call_requires_explicit_consume_declaration() {
        let permission_name = "service.review.comments";
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut provider = service_provider(provider_id, permission_name);
        let mut consumer = manifest(consumer_id);

        provider.bundle = Some(ExtensionBundleMembership {
            id: "com.example.bundle".to_string(),
            required: true,
        });
        consumer.bundle = Some(ExtensionBundleMembership {
            id: "com.example.bundle".to_string(),
            required: true,
        });

        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(provider, InstallOptions::default())
            .unwrap();
        registry
            .install(consumer, InstallOptions::default())
            .unwrap();

        let err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();
        assert!(matches!(err, RegistryError::ServiceDenied { .. }));

        let mut registry = ExtensionRegistry::in_memory();
        let mut provider = service_provider(provider_id, permission_name);
        let mut consumer = service_consumer(consumer_id, provider_id, permission_name);
        provider.bundle = Some(ExtensionBundleMembership {
            id: "com.example.bundle".to_string(),
            required: true,
        });
        consumer.bundle = provider.bundle.clone();

        registry
            .install(provider, InstallOptions::default())
            .unwrap();
        registry
            .install(consumer, InstallOptions::default())
            .unwrap();

        let grant = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap();
        assert_eq!(grant.required_permission, permission_name);
    }

    #[test]
    fn disabled_extension_cannot_participate_in_service_calls() {
        let permission_name = "service.review.comments";
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut registry = ExtensionRegistry::in_memory();

        registry
            .install(
                service_provider(provider_id, permission_name),
                InstallOptions::default(),
            )
            .unwrap();
        registry
            .install(
                service_consumer(consumer_id, provider_id, permission_name),
                InstallOptions::default(),
            )
            .unwrap();

        registry.disable(provider_id).unwrap();
        let provider_err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();
        assert!(matches!(provider_err, RegistryError::ServiceDenied { .. }));

        registry.enable(provider_id).unwrap();
        registry.disable(consumer_id).unwrap();
        let consumer_err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();
        assert!(matches!(consumer_err, RegistryError::ServiceDenied { .. }));
    }

    #[test]
    fn service_call_permission_mismatch_is_denied() {
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(
                service_provider(provider_id, "service.review.comments"),
                InstallOptions::default(),
            )
            .unwrap();
        registry
            .install(
                service_consumer(consumer_id, provider_id, "service.review.other"),
                InstallOptions::default(),
            )
            .unwrap();

        let err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();

        assert!(matches!(err, RegistryError::ServiceDenied { .. }));
    }

    #[test]
    fn service_call_permission_metadata_matching_is_required() {
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(
                service_provider(provider_id, "service.review.comments"),
                InstallOptions::default(),
            )
            .unwrap();
        registry
            .install(
                service_consumer(consumer_id, provider_id, "service.review.comments"),
                InstallOptions::default(),
            )
            .unwrap();

        let grant = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap();

        assert_eq!(grant.required_permission, "service.review.comments");
    }

    #[test]
    fn service_call_permission_metadata_description_mismatch_is_denied() {
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut registry = ExtensionRegistry::in_memory();
        let mut consumer = service_consumer(consumer_id, provider_id, "service.review.comments");
        consumer.permissions.insert(
            "service.review.comments".to_string(),
            permission("different description"),
        );

        registry
            .install(
                service_provider(provider_id, "service.review.comments"),
                InstallOptions::default(),
            )
            .unwrap();
        registry
            .install(consumer, InstallOptions::default())
            .unwrap();

        let err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();

        assert!(matches!(err, RegistryError::ServiceDenied { .. }));
    }

    #[test]
    fn service_call_permission_metadata_risk_tier_mismatch_is_denied() {
        let provider_id = "com.example.provider";
        let consumer_id = "com.example.consumer";
        let mut registry = ExtensionRegistry::in_memory();
        let mut consumer = service_consumer(consumer_id, provider_id, "service.review.comments");
        consumer.permissions.insert(
            "service.review.comments".to_string(),
            ExtensionPermission {
                description: "provide service".to_string(),
                risk_tier: Some("R1".to_string()),
            },
        );

        registry
            .install(
                service_provider(provider_id, "service.review.comments"),
                InstallOptions::default(),
            )
            .unwrap();
        registry
            .install(consumer, InstallOptions::default())
            .unwrap();

        let err = registry
            .authorize_service_call(service_call(consumer_id, provider_id))
            .unwrap_err();

        assert!(matches!(err, RegistryError::ServiceDenied { .. }));
    }

    #[test]
    fn rollback_restores_previous_active_version() {
        let mut registry = ExtensionRegistry::in_memory();
        let installed = registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();
        assert_eq!(
            installed.source,
            ExtensionSourceSnapshot::from_provenance(&manifest("com.example.extension").provenance)
        );

        let mut next = manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        let installed = registry.install(next, InstallOptions::default()).unwrap();
        assert_eq!(installed.previous_version.as_deref(), Some("1.0.0"));

        let rolled_back = registry.rollback("com.example.extension").unwrap();
        assert_eq!(rolled_back.version, "1.0.0");
        assert_eq!(
            registry
                .active_record("com.example.extension")
                .unwrap()
                .version,
            "1.0.0"
        );
    }

    #[test]
    fn extension_snapshot_rejects_self_referential_previous_version() {
        let first = manifest("com.example.extension");
        let snapshot = extension_snapshot(
            BTreeMap::from([(first.version.clone(), first.clone())]),
            BTreeMap::from([(
                first.version.clone(),
                extension_record(&first, Some(&first.version)),
            )]),
        );

        let err = snapshot.validate().unwrap_err();
        assert!(err.contains("self-referential previous_version"));
    }

    #[test]
    fn extension_snapshot_rejects_missing_previous_version_manifest() {
        let mut second = manifest("com.example.extension");
        second.version = "1.1.0".to_string();
        second.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        second.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        let snapshot = extension_snapshot(
            BTreeMap::from([(second.version.clone(), second.clone())]),
            BTreeMap::from([(
                second.version.clone(),
                extension_record(&second, Some("0.9.0")),
            )]),
        );

        let err = snapshot.validate().unwrap_err();
        assert!(err.contains("references missing previous_version"));
        assert!(err.contains("0.9.0"));
    }

    #[test]
    fn extension_snapshot_hydration_rejects_invalid_manifest() {
        let mut invalid = manifest("com.example.extension");
        invalid.name.clear();
        let snapshot = extension_snapshot(
            BTreeMap::from([(invalid.version.clone(), invalid.clone())]),
            BTreeMap::from([(invalid.version.clone(), extension_record(&invalid, None))]),
        );

        snapshot.validate().unwrap();
        let err = ExtensionRegistry::from_snapshot(snapshot, ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(err.contains("manifest validation failed"));
        assert!(err.contains("com.example.extension"));
    }

    #[test]
    fn extension_snapshot_hydration_rejects_boundary_mismatch() {
        let third_party = manifest("com.example.extension");
        let mut record = extension_record(&third_party, None);
        record.boundary = ExtensionBoundary::Official;
        let snapshot = extension_snapshot(
            BTreeMap::from([(third_party.version.clone(), third_party.clone())]),
            BTreeMap::from([(third_party.version.clone(), record)]),
        );

        snapshot.validate().unwrap();
        let err = ExtensionRegistry::from_snapshot(snapshot, ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(err.contains("boundary mismatch"));
    }

    #[test]
    fn install_rejects_source_provenance_change_without_approval() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let mut next = manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();
        next.provenance.registry_identity = Some("other-registry".to_string());

        let err = registry
            .install(next, InstallOptions::default())
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::SourceChangeRequiresApproval { extension_id }
                if extension_id == "com.example.extension"
        ));
    }

    #[test]
    fn install_accepts_source_provenance_change_with_approval() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let mut next = manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();
        next.provenance.registry_identity = Some("other-registry".to_string());

        let record = registry
            .install(
                next.clone(),
                InstallOptions::default().approve_source_change(),
            )
            .unwrap();
        assert_eq!(
            record.source,
            ExtensionSourceSnapshot::from_provenance(&next.provenance)
        );
    }

    #[test]
    fn same_github_repository_commit_update_does_not_require_source_approval() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(
                github_manifest("com.example.extension"),
                InstallOptions::default(),
            )
            .unwrap();

        let mut next = github_manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.commit = Some("2222222".to_string());
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        let record = registry.install(next, InstallOptions::default()).unwrap();
        assert_eq!(record.source.commit.as_deref(), Some("2222222"));
        assert_eq!(record.source.source_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            record.source.manifest_path.as_deref(),
            Some("atelia.package.yaml")
        );
    }

    #[test]
    fn github_source_reference_snapshot_trims_authority_fields() {
        let mut registry = ExtensionRegistry::in_memory();
        let mut first = github_manifest("com.example.extension");
        first.provenance.repository = Some(" https://github.com/example/package ".to_string());
        first.provenance.source_ref = Some(" refs/heads/main ".to_string());
        first.provenance.manifest_path = Some(" atelia.package.yaml ".to_string());

        let record = registry.install(first, InstallOptions::default()).unwrap();
        assert_eq!(
            record.source.repository.as_deref(),
            Some("https://github.com/example/package")
        );
        assert_eq!(record.source.source_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            record.source.manifest_path.as_deref(),
            Some("atelia.package.yaml")
        );

        let mut next = github_manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        registry.install(next, InstallOptions::default()).unwrap();
    }

    #[test]
    fn github_ref_or_manifest_path_change_requires_source_approval() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(
                github_manifest("com.example.extension"),
                InstallOptions::default(),
            )
            .unwrap();

        let mut changed_ref = github_manifest("com.example.extension");
        changed_ref.version = "1.1.0".to_string();
        changed_ref.provenance.source_ref = Some("refs/heads/release".to_string());
        changed_ref.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        changed_ref.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();

        let err = registry
            .install(changed_ref.clone(), InstallOptions::default())
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::SourceChangeRequiresApproval { .. }
        ));

        registry
            .install(
                changed_ref,
                InstallOptions::default().approve_source_change(),
            )
            .unwrap();

        let mut changed_manifest_path = github_manifest("com.example.extension");
        changed_manifest_path.version = "1.2.0".to_string();
        changed_manifest_path.provenance.source_ref = Some("refs/heads/release".to_string());
        changed_manifest_path.provenance.manifest_path = Some("packages/review.yaml".to_string());
        changed_manifest_path.provenance.manifest_digest = THIRD_MANIFEST_DIGEST.to_string();
        changed_manifest_path.provenance.artifact_digest = THIRD_ARTIFACT_DIGEST.to_string();

        let err = registry
            .install(changed_manifest_path, InstallOptions::default())
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::SourceChangeRequiresApproval { .. }
        ));
    }

    #[test]
    fn publication_state_change_does_not_require_source_approval() {
        let mut registry = ExtensionRegistry::in_memory();
        registry
            .install(manifest("com.example.extension"), InstallOptions::default())
            .unwrap();

        let mut next = manifest("com.example.extension");
        next.version = "1.1.0".to_string();
        next.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        next.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();
        next.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: ExtensionRegistrySubmission::Submitted,
        });

        let record = registry.install(next, InstallOptions::default()).unwrap();
        assert_eq!(
            record.source.publication,
            Some(ExtensionPublication {
                visibility: ExtensionPublicationVisibility::PublicSearchable,
                registry_submission: ExtensionRegistrySubmission::Submitted,
            })
        );
    }

    #[test]
    fn provenance_lineage_and_publication_are_validated_and_preserved() {
        let mut manifest = manifest("com.example.extension");
        manifest.provenance.lineage = Some(ExtensionLineage {
            parent_id: "com.example.parent".to_string(),
            parent_version: Some("1.2.3".to_string()),
            parent_manifest_digest: Some(OTHER_MANIFEST_DIGEST.to_string()),
            relationship: ExtensionLineageRelationship::Remix,
        });
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::UnlistedShare,
            registry_submission: ExtensionRegistrySubmission::NotSubmitted,
        });

        let validated = manifest
            .clone()
            .validate(&ManifestValidationPolicy::default())
            .unwrap();
        assert_eq!(
            validated.manifest.provenance.lineage,
            manifest.provenance.lineage
        );
        assert_eq!(
            validated.manifest.provenance.publication,
            manifest.provenance.publication
        );

        let serialized = serde_json::to_string(&validated.manifest).unwrap();
        assert!(serialized.contains("\"lineage\""));
        assert!(serialized.contains("\"publication\""));
    }

    #[test]
    fn private_remix_cannot_claim_registry_submission() {
        let mut manifest = manifest("local.example.extension");
        manifest.provenance.source = ProvenanceSource::Local;
        manifest.provenance.registry_identity = None;
        manifest.provenance.signature = None;
        manifest.provenance.signer = None;
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::PrivateRemix,
            registry_submission: ExtensionRegistrySubmission::Submitted,
        });

        let err = manifest
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "provenance.publication.registry_submission",
                ..
            }
        ));
    }

    #[test]
    fn private_remix_can_stay_local_and_unsubmitted() {
        let mut manifest = manifest("local.example.extension");
        manifest.provenance.source = ProvenanceSource::Local;
        manifest.provenance.registry_identity = None;
        manifest.provenance.signature = None;
        manifest.provenance.signer = None;
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::PrivateRemix,
            registry_submission: ExtensionRegistrySubmission::NotSubmitted,
        });

        let validated = manifest
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap();
        assert_eq!(
            validated.manifest.provenance.publication,
            manifest.provenance.publication
        );
    }

    #[test]
    fn private_remix_can_be_github_sourced_and_unsubmitted() {
        let mut manifest = github_manifest("com.example.extension");
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::PrivateRemix,
            registry_submission: ExtensionRegistrySubmission::NotSubmitted,
        });

        let validated = manifest
            .validate(&ManifestValidationPolicy::default())
            .unwrap();
        assert_eq!(
            validated.manifest.provenance.publication,
            manifest.provenance.publication
        );
    }

    #[test]
    fn unlisted_share_can_be_local_and_unsubmitted() {
        let mut manifest = manifest("local.example.extension");
        manifest.provenance.source = ProvenanceSource::Local;
        manifest.provenance.registry_identity = None;
        manifest.provenance.signature = None;
        manifest.provenance.signer = None;
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::UnlistedShare,
            registry_submission: ExtensionRegistrySubmission::NotSubmitted,
        });

        let validated = manifest
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap();
        assert_eq!(
            validated.manifest.provenance.publication,
            manifest.provenance.publication
        );
    }

    #[test]
    fn unlisted_share_rejects_rejected_submission() {
        let mut manifest = github_manifest("com.example.extension");
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::UnlistedShare,
            registry_submission: ExtensionRegistrySubmission::Rejected,
        });

        let err = manifest
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "provenance.publication.registry_submission",
                ..
            }
        ));
    }

    #[test]
    fn unlisted_share_requires_registry_identity_when_registry_submitted() {
        for registry_submission in [
            ExtensionRegistrySubmission::Submitted,
            ExtensionRegistrySubmission::Accepted,
        ] {
            let mut manifest = github_manifest("com.example.extension");
            manifest.provenance.publication = Some(ExtensionPublication {
                visibility: ExtensionPublicationVisibility::UnlistedShare,
                registry_submission,
            });

            let err = manifest
                .validate(&ManifestValidationPolicy::default())
                .unwrap_err();
            assert!(matches!(
                err,
                ExtensionValidationError::ProvenanceRequired {
                    field: "provenance.registry_identity",
                    ..
                }
            ));
        }
    }

    #[test]
    fn public_searchable_allows_submitted_or_accepted_with_registry_identity() {
        for registry_submission in [
            ExtensionRegistrySubmission::Submitted,
            ExtensionRegistrySubmission::Accepted,
        ] {
            let mut manifest = github_manifest("com.example.extension");
            manifest.provenance.registry_identity = Some("third-party-registry".to_string());
            manifest.provenance.publication = Some(ExtensionPublication {
                visibility: ExtensionPublicationVisibility::PublicSearchable,
                registry_submission,
            });

            let validated = manifest
                .validate(&ManifestValidationPolicy::default())
                .unwrap();
            assert_eq!(
                validated.manifest.provenance.publication,
                manifest.provenance.publication
            );
        }
    }

    #[test]
    fn public_searchable_rejects_not_submitted_and_rejected_states() {
        for registry_submission in [
            ExtensionRegistrySubmission::NotSubmitted,
            ExtensionRegistrySubmission::Rejected,
        ] {
            let mut manifest = github_manifest("com.example.extension");
            manifest.provenance.registry_identity = Some("third-party-registry".to_string());
            manifest.provenance.publication = Some(ExtensionPublication {
                visibility: ExtensionPublicationVisibility::PublicSearchable,
                registry_submission,
            });

            let err = manifest
                .validate(&ManifestValidationPolicy::default())
                .unwrap_err();
            assert!(matches!(
                err,
                ExtensionValidationError::InvalidField {
                    field: "provenance.publication.registry_submission",
                    ..
                }
            ));
        }
    }

    #[test]
    fn public_searchable_rejects_local_development_authority() {
        let mut manifest = manifest("local.example.extension");
        manifest.provenance.source = ProvenanceSource::Local;
        manifest.provenance.registry_identity = Some("third-party-registry".to_string());
        manifest.provenance.signature = None;
        manifest.provenance.signer = None;
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: ExtensionRegistrySubmission::Submitted,
        });

        let err = manifest
            .validate(&ManifestValidationPolicy::default().with_local_unsigned())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::BoundaryViolation { .. }
        ));
    }

    #[test]
    fn official_publication_requires_official_identity_and_accepted_submission() {
        let mut manifest = manifest("ai.atelia.example");
        manifest.provenance.registry_identity = Some("atelia-official".to_string());
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::Official,
            registry_submission: ExtensionRegistrySubmission::Submitted,
        });

        let err = manifest
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "provenance.publication.registry_submission",
                ..
            }
        ));
    }

    #[test]
    fn official_publication_rejects_non_official_registry_identity_even_when_accepted() {
        let mut manifest = manifest("ai.atelia.example");
        manifest.provenance.registry_identity = Some("third-party-registry".to_string());
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::Official,
            registry_submission: ExtensionRegistrySubmission::Accepted,
        });

        let err = manifest
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::BoundaryViolation { .. }
        ));
    }

    #[test]
    fn official_publication_is_allowed_with_official_identity_and_accepted_submission() {
        let mut manifest = manifest("ai.atelia.example");
        manifest.provenance.registry_identity = Some("atelia-official".to_string());
        manifest.provenance.publication = Some(ExtensionPublication {
            visibility: ExtensionPublicationVisibility::Official,
            registry_submission: ExtensionRegistrySubmission::Accepted,
        });

        let validated = manifest
            .validate(&ManifestValidationPolicy::default())
            .unwrap();
        assert_eq!(validated.boundary, ExtensionBoundary::Official);
        assert_eq!(
            validated.manifest.provenance.publication,
            manifest.provenance.publication
        );
    }

    #[test]
    fn extension_service_install_status_and_list_returns_installed_extensions() {
        let mut service = ExtensionRegistryService::new();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.extension"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.other"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();

        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.extension".to_string(),
            })
            .unwrap();
        let status_record = status.record.expect("status should include a record");
        assert_eq!(status_record.version, "1.0.0");
        assert_eq!(status_record.status, ExtensionInstallStatus::Installed);

        let list = service
            .list_extensions(ListExtensionsRequest::default())
            .unwrap();
        assert_eq!(list.extensions.len(), 2);

        let ids: std::collections::HashSet<_> = list
            .extensions
            .iter()
            .map(|entry| entry.extension_id.as_str())
            .collect();
        assert!(ids.contains("com.example.extension"));
        assert!(ids.contains("com.example.other"));
    }

    #[test]
    fn extension_service_rollback_restores_previous_extension_version() {
        let mut service = ExtensionRegistryService::new();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.extension"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();

        let mut updated = manifest("com.example.extension");
        updated.version = "1.1.0".to_string();
        updated.provenance.manifest_digest = OTHER_MANIFEST_DIGEST.to_string();
        updated.provenance.artifact_digest = OTHER_ARTIFACT_DIGEST.to_string();
        service
            .install_extension(InstallExtensionRequest {
                manifest: updated,
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();

        let rolled_back = service
            .rollback_extension(RollbackExtensionRequest {
                extension_id: "com.example.extension".to_string(),
            })
            .unwrap();
        assert_eq!(rolled_back.record.version, "1.0.0");

        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.extension".to_string(),
            })
            .unwrap();
        let status_record = status.record.expect("status should include a record");
        assert_eq!(status_record.version, "1.0.0");
        assert_eq!(
            status_record.status,
            ExtensionInstallStatus::InstalledPreviousVersion
        );
    }

    #[test]
    fn extension_service_blocked_install_and_status_are_reported_explicitly() {
        let mut service = ExtensionRegistryService::new();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.extension"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();

        service
            .apply_blocklist(ApplyBlocklistRequest {
                entry: BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.extension".to_string()),
                    reason: BlockReason::ManifestMismatch,
                    note: Some("policy update".to_string()),
                },
            })
            .unwrap();

        let err = service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.extension"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::Blocked {
                reason: BlockReason::ManifestMismatch,
                ..
            }
        ));

        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.extension".to_string(),
            })
            .unwrap();
        let status_record = status.record.expect("status should include a record");
        let block = status.block.expect("status should expose block reason");

        assert_eq!(status_record.status, ExtensionInstallStatus::Blocked);
        assert_eq!(block.reason, BlockReason::ManifestMismatch);

        let list = service
            .list_extensions(ListExtensionsRequest::default())
            .unwrap();
        let listed_status = list
            .extensions
            .iter()
            .find(|entry| entry.extension_id == "com.example.extension")
            .and_then(|entry| entry.record.as_ref())
            .expect("extension should still be listed");
        assert_eq!(listed_status.status, ExtensionInstallStatus::Blocked);
    }

    #[test]
    fn extension_manifest_serializes_empty_tools_as_missing_field() {
        let mut extension = manifest("com.example.empty-tools");
        extension.tools.clear();

        let serialized = serde_json::to_value(&extension).unwrap();

        assert!(
            serialized.get("tools").is_none(),
            "tools field should be omitted when empty"
        );

        let deserialized: ExtensionManifest =
            serde_json::from_value(serialized).expect("missing tools should default to empty");

        assert!(deserialized.tools.is_empty());
    }

    #[test]
    fn extension_manifest_roundtrips_extended_sections_with_defaults() {
        let mut extension = manifest("com.example.extended-sections");
        extension.tool_output.push(tool_output_definition("ping"));
        extension.hooks.push(hook_definition("hk_review"));
        extension.webhooks.push(webhook_definition("wh_review"));
        extension
            .composition
            .attachments
            .push(ExtensionCompositionAttachment {
                extension_id: "com.example.partner".to_string(),
                required: Some(true),
            });
        extension.migration.from.push("1.0.0".to_string());
        extension.migration.notes = Some("backfills tool output defaults".to_string());

        let serialized = serde_json::to_value(&extension).unwrap();

        assert!(serialized.get("tool_output").is_some());
        assert!(serialized.get("hooks").is_some());
        assert!(serialized.get("webhooks").is_some());
        assert!(serialized.get("composition").is_some());
        assert!(serialized.get("migration").is_some());

        let deserialized: ExtensionManifest = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.tool_output.len(), 1);
        assert_eq!(deserialized.hooks.len(), 1);
        assert_eq!(deserialized.webhooks.len(), 1);
        assert_eq!(deserialized.composition.attachments.len(), 1);
        assert_eq!(deserialized.migration.from, vec!["1.0.0".to_string()]);
        assert_eq!(
            deserialized.migration.notes.as_deref(),
            Some("backfills tool output defaults")
        );
    }

    #[test]
    fn extension_manifest_deserializes_missing_extended_sections_as_defaults() {
        let extension = manifest("com.example.missing-sections");
        let mut serialized = serde_json::to_value(&extension).unwrap();
        let object = serialized
            .as_object_mut()
            .expect("manifest should serialize to an object");
        object.remove("tool_output");
        object.remove("hooks");
        object.remove("webhooks");
        object.remove("composition");
        object.remove("migration");

        let deserialized: ExtensionManifest = serde_json::from_value(serialized).unwrap();
        assert!(deserialized.tool_output.is_empty());
        assert!(deserialized.hooks.is_empty());
        assert!(deserialized.webhooks.is_empty());
        assert!(deserialized.composition.attachments.is_empty());
        assert!(deserialized.migration.from.is_empty());
        assert!(deserialized.migration.notes.is_none());
    }

    #[test]
    fn nested_section_arrays_deserialize_as_defaults_when_omitted() {
        let mut extension = manifest("com.example.missing-nested-arrays");
        extension.tool_output.push(tool_output_definition("ping"));
        extension.hooks.push(hook_definition("hk_review"));
        extension.webhooks.push(webhook_definition("wh_review"));
        extension
            .composition
            .attachments
            .push(ExtensionCompositionAttachment {
                extension_id: "com.example.partner".to_string(),
                required: Some(true),
            });
        extension.migration.from.push("1.0.0".to_string());

        let mut serialized = serde_json::to_value(&extension).unwrap();
        let object = serialized
            .as_object_mut()
            .expect("manifest should serialize to an object");

        object
            .get_mut("tool_output")
            .and_then(|value| value.as_array_mut())
            .expect("tool_output should be an array")[0]
            .as_object_mut()
            .expect("tool_output entry should be an object")
            .remove("fields");
        object
            .get_mut("tool_output")
            .and_then(|value| value.as_array_mut())
            .expect("tool_output should be an array")[0]
            .as_object_mut()
            .expect("tool_output entry should be an object")
            .remove("redactions");
        object
            .get_mut("hooks")
            .and_then(|value| value.as_array_mut())
            .expect("hooks should be an array")[0]
            .as_object_mut()
            .expect("hook entry should be an object")
            .remove("required_capabilities");
        object
            .get_mut("webhooks")
            .and_then(|value| value.as_array_mut())
            .expect("webhooks should be an array")[0]
            .as_object_mut()
            .expect("webhook entry should be an object")
            .remove("required_capabilities");
        object
            .get_mut("composition")
            .and_then(|value| value.as_object_mut())
            .expect("composition should be an object")
            .remove("attachments");
        object
            .get_mut("migration")
            .and_then(|value| value.as_object_mut())
            .expect("migration should be an object")
            .remove("from");

        let deserialized: ExtensionManifest = serde_json::from_value(serialized).unwrap();
        assert!(deserialized.tool_output[0].fields.is_empty());
        assert!(deserialized.tool_output[0].redactions.is_empty());
        assert!(deserialized.hooks[0].required_capabilities.is_empty());
        assert!(deserialized.webhooks[0].required_capabilities.is_empty());
        assert!(deserialized.composition.attachments.is_empty());
        assert!(deserialized.migration.from.is_empty());
    }

    #[test]
    fn composition_attachment_extension_ids_must_use_reverse_dns_namespaces() {
        let mut extension = manifest("com.example.invalid-composition");
        extension
            .composition
            .attachments
            .push(ExtensionCompositionAttachment {
                extension_id: "not-a-reverse-dns-id".to_string(),
                required: Some(true),
            });

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "composition.attachments.extension_id",
                ..
            }
        ));
    }

    #[test]
    fn migration_from_versions_must_be_valid_semver() {
        let mut extension = manifest("com.example.invalid-migration");
        extension.migration.from.push("1.0".to_string());

        let err = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionValidationError::InvalidField {
                field: "migration.from",
                ..
            }
        ));
    }

    #[test]
    fn extension_kind_roundtrips_extended_taxonomy_and_aliases() {
        let kinds = vec![
            ExtensionKind::Tool,
            ExtensionKind::Service,
            ExtensionKind::HookProvider,
            ExtensionKind::WebhookReceiver,
            ExtensionKind::ToolOutputCustomizer,
            ExtensionKind::Workflow,
            ExtensionKind::Notification,
            ExtensionKind::ApprovalAgent,
            ExtensionKind::Review,
            ExtensionKind::ReviewAgent,
            ExtensionKind::AgentProvider,
            ExtensionKind::DelegatedAgent,
            ExtensionKind::MemoryProvider,
            ExtensionKind::MemoryStrategy,
            ExtensionKind::Integration,
            ExtensionKind::Presentation,
        ];

        let serialized = serde_json::to_value(&kinds).unwrap();
        let deserialized: Vec<ExtensionKind> = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(deserialized, kinds);

        let legacy_aliases: Vec<ExtensionKind> = serde_json::from_value(serde_json::json!([
            "client_surface",
            "delegated_agent_provider"
        ]))
        .unwrap();
        assert_eq!(
            legacy_aliases,
            vec![ExtensionKind::Presentation, ExtensionKind::DelegatedAgent]
        );

        assert_eq!(
            serialized,
            serde_json::json!([
                "tool",
                "service",
                "hook_provider",
                "webhook_receiver",
                "tool_output_customizer",
                "workflow",
                "notification",
                "approval_agent",
                "review",
                "review_agent",
                "agent_provider",
                "delegated_agent",
                "memory_provider",
                "memory_strategy",
                "integration",
                "presentation"
            ])
        );
    }

    #[test]
    fn extended_taxonomy_manifest_validates_with_new_sections() {
        let mut extension = manifest("com.example.taxonomy");
        extension.types = vec![
            ExtensionKind::Tool,
            ExtensionKind::Service,
            ExtensionKind::ToolOutputCustomizer,
            ExtensionKind::HookProvider,
            ExtensionKind::WebhookReceiver,
            ExtensionKind::Workflow,
            ExtensionKind::Notification,
            ExtensionKind::ApprovalAgent,
            ExtensionKind::Review,
            ExtensionKind::ReviewAgent,
            ExtensionKind::AgentProvider,
            ExtensionKind::DelegatedAgent,
            ExtensionKind::MemoryProvider,
            ExtensionKind::MemoryStrategy,
            ExtensionKind::Integration,
            ExtensionKind::Presentation,
        ];
        extension.permissions.insert(
            "service.review.comments".to_string(),
            permission("provide service"),
        );
        extension
            .services
            .provides
            .push(ExtensionServiceDefinition {
                service: "review.comments".to_string(),
                method: "summarize".to_string(),
                schema_version: "v1".to_string(),
                required_permission: "service.review.comments".to_string(),
            });
        extension.tool_output.push(tool_output_definition("ping"));
        extension.hooks.push(hook_definition("hk_review"));
        extension.webhooks.push(webhook_definition("wh_review"));

        let validated = extension
            .validate(&ManifestValidationPolicy::default())
            .unwrap();
        assert_eq!(validated.boundary, ExtensionBoundary::ThirdParty);
    }

    #[test]
    fn extended_taxonomy_manifest_installs_without_legacy_sections() {
        let mut extension = manifest("com.example.legacy-taxonomy");
        extension.types = vec![
            ExtensionKind::Tool,
            ExtensionKind::ToolOutputCustomizer,
            ExtensionKind::HookProvider,
            ExtensionKind::WebhookReceiver,
        ];

        let mut serialized = serde_json::to_value(&extension).unwrap();
        let object = serialized
            .as_object_mut()
            .expect("manifest should serialize to an object");
        object.remove("tool_output");
        object.remove("hooks");
        object.remove("webhooks");

        let legacy_manifest: ExtensionManifest = serde_json::from_value(serialized).unwrap();

        assert!(legacy_manifest.tool_output.is_empty());
        assert!(legacy_manifest.hooks.is_empty());
        assert!(legacy_manifest.webhooks.is_empty());

        let mut registry = ExtensionRegistry::in_memory();
        let record = registry
            .install(legacy_manifest.clone(), InstallOptions::default())
            .unwrap();

        assert_eq!(record.id, legacy_manifest.id);
        assert_eq!(record.version, legacy_manifest.version);
        assert_eq!(
            registry
                .active_record(&legacy_manifest.id)
                .expect("installed extension should be active")
                .version,
            legacy_manifest.version
        );
    }

    #[test]
    fn list_extensions_request_deserializes_missing_include_blocked_as_true() {
        let request: ListExtensionsRequest = serde_json::from_str("{}").unwrap();

        assert!(request.include_blocked);
        assert_eq!(request, ListExtensionsRequest::default());
    }

    #[test]
    fn list_extensions_request_deserializes_include_blocked_false() {
        let request: ListExtensionsRequest =
            serde_json::from_str("{\"include_blocked\":false}").unwrap();

        assert!(!request.include_blocked);
        assert_ne!(request, ListExtensionsRequest::default());

        let mut service = ExtensionRegistryService::new();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.extension"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();
        service
            .install_extension(InstallExtensionRequest {
                manifest: manifest("com.example.other"),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap();

        service
            .apply_blocklist(ApplyBlocklistRequest {
                entry: BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.extension".to_string()),
                    reason: BlockReason::PolicyViolation,
                    note: None,
                },
            })
            .unwrap();

        let list = service.list_extensions(request).unwrap();
        assert_eq!(list.extensions.len(), 1);
        assert_eq!(list.extensions[0].extension_id, "com.example.other");
    }

    #[test]
    fn install_extension_request_deserializes_manifest_only_with_false_defaults() {
        let request: InstallExtensionRequest = serde_json::from_value(serde_json::json!({
            "manifest": manifest("com.example.extension"),
        }))
        .unwrap();

        assert_eq!(
            request,
            InstallExtensionRequest::with_defaults(manifest("com.example.extension"))
        );
        assert_eq!(InstallOptions::from(request), InstallOptions::default());
    }

    #[test]
    fn extension_service_blocklist_listing_works() {
        let mut service = ExtensionRegistryService::new();
        service
            .apply_blocklist(ApplyBlocklistRequest {
                entry: BlocklistEntry {
                    key: BlockKey::PermissionPattern("test.*".to_string()),
                    reason: BlockReason::PolicyViolation,
                    note: None,
                },
            })
            .unwrap();

        let list = service.list_blocklist(ListBlocklistRequest {}).unwrap();
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].reason, BlockReason::PolicyViolation);
    }
}
