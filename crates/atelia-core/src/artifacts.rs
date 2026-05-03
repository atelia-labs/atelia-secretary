//! Local artifact storage and spillover helpers for large tool outputs.

use crate::domain::{
    OutputRef, OutputRefId, StructuredValue, ToolResult, ToolResultField, TruncationMetadata,
};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const DEFAULT_ARTIFACT_APP_DIR: &str = "atelia-secretary";
const DEFAULT_ARTIFACT_DIR: &str = "artifacts";
const DEFAULT_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStoreConfig {
    pub root_dir: PathBuf,
}

impl ArtifactStoreConfig {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    pub fn default_local() -> Self {
        let root_dir = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_ARTIFACT_APP_DIR)
            .join(DEFAULT_ARTIFACT_DIR);

        Self { root_dir }
    }
}

#[derive(Debug)]
pub enum ArtifactError {
    InvalidScope { scope: String },
    Io(io::Error),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScope { scope } => write!(f, "invalid artifact scope: {scope}"),
            Self::Io(error) => write!(f, "artifact io failed: {error}"),
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidScope { .. } => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for ArtifactError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type ArtifactResult<T> = Result<T, ArtifactError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArtifactStore {
    config: ArtifactStoreConfig,
}

impl LocalArtifactStore {
    pub fn new(config: ArtifactStoreConfig) -> Self {
        Self { config }
    }

    pub fn default_local() -> Self {
        Self::new(ArtifactStoreConfig::default_local())
    }

    pub fn root_dir(&self) -> &Path {
        &self.config.root_dir
    }

    pub fn write_bytes(
        &self,
        scope: &str,
        label: impl AsRef<str>,
        media_type: impl Into<String>,
        bytes: &[u8],
    ) -> ArtifactResult<OutputRef> {
        let scope_dir_name = sanitize_segment(scope)?;
        let label = label.as_ref();
        let safe_label = sanitize_label(label);
        let id = OutputRefId::new();
        let file_name = format!("{}-{safe_label}.artifact", id.as_str());
        let dir = self.config.root_dir.join(scope_dir_name);
        let path = dir.join(file_name);

        fs::create_dir_all(&dir)?;
        fs::write(&path, bytes)?;

        Ok(OutputRef {
            id,
            uri: path.to_string_lossy().into_owned(),
            media_type: media_type.into(),
            label: Some(label.to_string()),
            digest: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultSpilloverOptions {
    pub max_inline_bytes: usize,
    pub media_type: String,
}

impl ToolResultSpilloverOptions {
    pub fn new(max_inline_bytes: usize) -> Self {
        Self {
            max_inline_bytes,
            media_type: DEFAULT_MEDIA_TYPE.to_string(),
        }
    }

    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = media_type.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultSpilloverReport {
    pub spilled_fields: Vec<String>,
    pub original_bytes: u64,
    pub retained_bytes: u64,
}

pub fn spill_large_tool_result_fields(
    result: &mut ToolResult,
    store: &LocalArtifactStore,
    scope: &str,
    options: &ToolResultSpilloverOptions,
) -> ArtifactResult<Option<ToolResultSpilloverReport>> {
    if options.max_inline_bytes == 0 {
        return Ok(None);
    }

    let mut spilled_fields = Vec::new();
    let mut original_bytes = 0u64;
    let mut retained_bytes = 0u64;
    let mut output_refs = Vec::new();

    for field in &mut result.fields {
        let Some(bytes) = spillable_field_bytes(field) else {
            continue;
        };

        if bytes.len() <= options.max_inline_bytes {
            continue;
        }

        let output_ref = store.write_bytes(
            scope,
            format!("{}.{}", result.tool_id, field.key),
            options.media_type.clone(),
            &bytes,
        )?;
        let replacement = format!("artifact_ref {}", output_ref.uri);

        original_bytes += bytes.len() as u64;
        retained_bytes += replacement.len() as u64;
        spilled_fields.push(field.key.clone());
        field.value = StructuredValue::String(replacement);
        output_refs.push(output_ref);
    }

    if spilled_fields.is_empty() {
        return Ok(None);
    }

    result.output_refs.extend(output_refs);
    result.truncation = Some(merge_truncation(
        result.truncation.take(),
        original_bytes,
        retained_bytes,
    ));

    Ok(Some(ToolResultSpilloverReport {
        spilled_fields,
        original_bytes,
        retained_bytes,
    }))
}

fn spillable_field_bytes(field: &ToolResultField) -> Option<Vec<u8>> {
    match &field.value {
        StructuredValue::String(value) => Some(value.as_bytes().to_vec()),
        StructuredValue::StringList(values) => Some(values.join("\n").into_bytes()),
        StructuredValue::Null | StructuredValue::Bool(_) | StructuredValue::Integer(_) => None,
    }
}

fn merge_truncation(
    existing: Option<TruncationMetadata>,
    original_bytes: u64,
    retained_bytes: u64,
) -> TruncationMetadata {
    match existing {
        Some(existing) => TruncationMetadata {
            original_bytes: existing.original_bytes.saturating_add(original_bytes),
            retained_bytes: existing.retained_bytes.saturating_add(retained_bytes),
            reason: format!("{}; artifact spillover", existing.reason),
        },
        None => TruncationMetadata {
            original_bytes,
            retained_bytes,
            reason: "artifact spillover".to_string(),
        },
    }
}

fn sanitize_segment(value: &str) -> ArtifactResult<String> {
    let sanitized = sanitize_label(value);
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(ArtifactError::InvalidScope {
            scope: value.to_string(),
        });
    }
    Ok(sanitized)
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LedgerTimestamp, ToolInvocationId, ToolResultId, ToolResultStatus};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("atelia-artifacts-{name}-{unique}"))
    }

    fn result_with_field(key: &str, value: StructuredValue) -> ToolResult {
        ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            invocation_id: ToolInvocationId::new(),
            tool_id: "fs.search".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("tool_result.test.v1".to_string()),
            fields: vec![ToolResultField {
                key: key.to_string(),
                value,
            }],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        }
    }

    #[test]
    fn writes_artifact_under_scoped_directory() {
        let root = temp_root("write");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));

        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        assert!(reference.uri.starts_with(root.to_str().unwrap()));
        assert!(reference.uri.contains("repo_example"));
        assert_eq!(Some("search output"), reference.label.as_deref());
        assert_eq!("hello", fs::read_to_string(reference.uri).unwrap());
    }

    #[test]
    fn spills_large_string_field_to_output_ref() {
        let root = temp_root("spill");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let mut result = result_with_field("matches", StructuredValue::String("abcdef".into()));

        let report = spill_large_tool_result_fields(
            &mut result,
            &store,
            "repo_example",
            &ToolResultSpilloverOptions::new(4),
        )
        .unwrap()
        .unwrap();

        assert_eq!(vec!["matches".to_string()], report.spilled_fields);
        assert_eq!(1, result.output_refs.len());
        assert_eq!(
            Some("artifact spillover"),
            result.truncation.as_ref().map(|t| t.reason.as_str())
        );
        assert_eq!(
            "abcdef",
            fs::read_to_string(&result.output_refs[0].uri).unwrap()
        );
        match &result.fields[0].value {
            StructuredValue::String(value) => assert!(value.starts_with("artifact_ref ")),
            other => panic!("expected replacement string, got {other:?}"),
        }
    }

    #[test]
    fn does_not_spill_small_field() {
        let root = temp_root("small");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(root));
        let mut result = result_with_field("summary", StructuredValue::String("abc".into()));

        let report = spill_large_tool_result_fields(
            &mut result,
            &store,
            "repo_example",
            &ToolResultSpilloverOptions::new(4),
        )
        .unwrap();

        assert!(report.is_none());
        assert!(result.output_refs.is_empty());
        assert!(result.truncation.is_none());
    }

    #[test]
    fn rejects_empty_artifact_scope() {
        let root = temp_root("scope");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(root));

        let error = store
            .write_bytes("", "label", "text/plain", b"x")
            .unwrap_err();

        assert!(matches!(error, ArtifactError::InvalidScope { .. }));
    }
}
