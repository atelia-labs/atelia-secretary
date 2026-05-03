//! Local artifact storage and spillover helpers for large tool outputs.

use crate::domain::{
    OutputRef, OutputRefId, StructuredValue, ToolResult, ToolResultField, TruncationMetadata,
};
#[cfg(test)]
use std::cell::Cell;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const WRITE_FILE_TMP_PREFIX: &str = ".atelia-artifact-tmp";
static WRITE_FILE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

trait ArtifactWriter {
    fn write_artifact_bytes(
        &self,
        scope: &str,
        label: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> ArtifactResult<OutputRef>;

    fn delete_artifact_bytes(&self, _output_ref: &OutputRef) -> ArtifactResult<()> {
        Ok(())
    }
}

impl ArtifactWriter for LocalArtifactStore {
    fn write_artifact_bytes(
        &self,
        scope: &str,
        label: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> ArtifactResult<OutputRef> {
        self.write_bytes(scope, label, media_type, bytes)
    }

    fn delete_artifact_bytes(&self, output_ref: &OutputRef) -> ArtifactResult<()> {
        self.delete_artifact(output_ref)
    }
}

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

        create_scope_dir(&dir)?;
        write_file_bytes(&path, bytes)?;

        Ok(OutputRef {
            id,
            uri: path.to_string_lossy().into_owned(),
            media_type: media_type.into(),
            label: Some(label.to_string()),
            digest: None,
        })
    }

    pub fn delete_artifact(&self, output_ref: &OutputRef) -> ArtifactResult<()> {
        let path = Path::new(&output_ref.uri);
        if !path.exists() {
            return Ok(());
        }

        let root = self.canonical_root_dir().map_err(ArtifactError::Io)?;

        let resolved_path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(ArtifactError::Io(error)),
        };

        if !resolved_path.starts_with(&root) {
            return Ok(());
        }

        match fs::remove_file(resolved_path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn canonical_root_dir(&self) -> io::Result<PathBuf> {
        match self.config.root_dir.canonicalize() {
            Ok(root) => Ok(root),
            Err(_) if self.config.root_dir.is_absolute() => Ok(self.config.root_dir.clone()),
            Err(_) => std::env::current_dir().map(|cwd| cwd.join(&self.config.root_dir)),
        }
    }
}

fn create_scope_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        builder.create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }

    #[cfg(not(unix))]
    fs::create_dir_all(path)
}

fn write_file_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_file_bytes_inner(path, bytes, false)
}

#[cfg(test)]
fn write_file_bytes_with_injected_failure(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_file_bytes_inner(path, bytes, true)
}

fn write_file_bytes_inner(path: &Path, bytes: &[u8], fail_after_write: bool) -> io::Result<()> {
    let mut open_result = None;

    for attempt in 0..64 {
        let temp_path = temporary_file_path(path, attempt);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        {
            options.mode(0o600);
        }

        match options.open(&temp_path) {
            Ok(file) => {
                open_result = Some((file, temp_path));
                break;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    let (mut file, temp_path) = open_result.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create temporary artifact file",
        )
    })?;

    let cleanup_temp_file = || {
        let _ = fs::remove_file(&temp_path);
    };

    use std::io::Write;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        cleanup_temp_file();
        return Err(error);
    }

    if fail_after_write {
        drop(file);
        cleanup_temp_file();
        return Err(io::Error::other("simulated post-write failure"));
    }

    if let Err(error) = file.flush() {
        drop(file);
        cleanup_temp_file();
        return Err(error);
    }

    #[cfg(unix)]
    {
        if let Err(error) = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)) {
            drop(file);
            cleanup_temp_file();
            return Err(error);
        }
    }

    drop(file);

    if let Err(error) = rename_atomic(&temp_path, path) {
        cleanup_temp_file();
        return Err(error);
    }

    Ok(())
}

fn temporary_file_path(path: &Path, attempt: u32) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let suffix = WRITE_FILE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        "{WRITE_FILE_TMP_PREFIX}-{file_name}-{attempt}-{suffix}"
    ))
}

fn rename_atomic(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        #[cfg(not(unix))]
        Err(error) => match error.kind() {
            ErrorKind::AlreadyExists => {
                fs::remove_file(destination)?;
                fs::rename(source, destination)
            }
            _ => Err(error),
        },
        #[cfg(unix)]
        Err(error) => Err(error),
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
    spill_large_tool_result_fields_with_writer(result, store, scope, options)
}

fn spill_large_tool_result_fields_with_writer(
    result: &mut ToolResult,
    store: &impl ArtifactWriter,
    scope: &str,
    options: &ToolResultSpilloverOptions,
) -> ArtifactResult<Option<ToolResultSpilloverReport>> {
    if options.max_inline_bytes == 0 {
        return Ok(None);
    }

    let mut original_bytes = 0u64;
    let mut retained_bytes = 0u64;
    let mut written_output_refs: Vec<OutputRef> = Vec::new();
    let mut planned_spills: Vec<(
        usize,     // field index
        String,    // field key
        String,    // replacement value
        OutputRef, // temp output reference
    )> = Vec::new();

    for (index, field) in result.fields.iter().enumerate() {
        let Some(bytes) = spillable_field_bytes(field) else {
            continue;
        };

        if bytes.len() <= options.max_inline_bytes {
            continue;
        }

        let output_ref = match store.write_artifact_bytes(
            scope,
            &format!("{}.{}", result.tool_id, field.key),
            &options.media_type,
            &bytes,
        ) {
            Ok(output_ref) => {
                written_output_refs.push(output_ref.clone());
                output_ref
            }
            Err(error) => {
                rollback_artifact_writes(store, &written_output_refs);
                return Err(error);
            }
        };
        let replacement = format!("artifact_ref {}", output_ref.uri);

        original_bytes += bytes.len() as u64;
        retained_bytes += replacement.len() as u64;
        planned_spills.push((index, field.key.clone(), replacement, output_ref));
    }

    if planned_spills.is_empty() {
        return Ok(None);
    }

    let spilled_fields: Vec<String> = planned_spills
        .iter()
        .map(|(_, key, _, _)| key.clone())
        .collect();

    for (index, _key, replacement, _output_ref) in &planned_spills {
        result.fields[*index].value = StructuredValue::String(replacement.clone());
    }

    result.output_refs.extend(
        planned_spills
            .into_iter()
            .map(|(_, _, _, output_ref)| output_ref),
    );
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

fn rollback_artifact_writes(writer: &impl ArtifactWriter, output_refs: &[OutputRef]) {
    for output_ref in output_refs {
        let _ = writer.delete_artifact_bytes(output_ref);
    }
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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
    fn write_file_bytes_cleans_partial_artifact_on_post_write_failure() {
        let root = temp_root("spill-partial");
        let dir = root.join("repo_example");
        create_scope_dir(&dir).unwrap();
        let path = dir.join("failed.artifact");

        let error = write_file_bytes_with_injected_failure(&path, b"hello").unwrap_err();
        assert!(matches!(error.kind(), io::ErrorKind::Other));
        assert!(!path.exists());
        assert!(
            fs::read_dir(&dir).unwrap().next().is_none(),
            "directory should contain no artifacts after injected failure",
        );
    }

    #[test]
    fn delete_artifact_ignores_path_outside_root() {
        let root = temp_root("delete-outside-root");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let outside_root = temp_root("delete-outside-root-target");
        let outside_dir = outside_root.join("other_scope");
        fs::create_dir_all(&outside_dir).unwrap();

        let outside_artifact = outside_dir.join("outside.artifact");
        fs::write(&outside_artifact, b"don't touch").unwrap();

        let forged_output_ref = OutputRef {
            id: OutputRefId::new(),
            uri: outside_artifact.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("outside".to_string()),
            digest: None,
        };

        store.delete_artifact(&forged_output_ref).unwrap();

        assert!(outside_artifact.exists());
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

    #[derive(Debug)]
    struct FailingWriter {
        writes: Cell<usize>,
        fail_on: usize,
    }

    impl FailingWriter {
        fn new(fail_on: usize) -> Self {
            Self {
                writes: Cell::new(0),
                fail_on,
            }
        }
    }

    #[derive(Debug)]
    struct FailingLocalWriter {
        delegate: LocalArtifactStore,
        writes: Cell<usize>,
        fail_on: usize,
    }

    impl FailingLocalWriter {
        fn new(delegate: LocalArtifactStore, fail_on: usize) -> Self {
            Self {
                delegate,
                writes: Cell::new(0),
                fail_on,
            }
        }
    }

    impl ArtifactWriter for FailingLocalWriter {
        fn write_artifact_bytes(
            &self,
            scope: &str,
            label: &str,
            media_type: &str,
            bytes: &[u8],
        ) -> ArtifactResult<OutputRef> {
            let write_count = self.writes.get() + 1;
            self.writes.set(write_count);

            if write_count == self.fail_on {
                return Err(ArtifactError::Io(std::io::Error::other(
                    "simulated write failure",
                )));
            }

            self.delegate
                .write_artifact_bytes(scope, label, media_type, bytes)
        }

        fn delete_artifact_bytes(&self, output_ref: &OutputRef) -> ArtifactResult<()> {
            self.delegate.delete_artifact(output_ref)
        }
    }

    impl ArtifactWriter for FailingWriter {
        fn write_artifact_bytes(
            &self,
            scope: &str,
            label: &str,
            media_type: &str,
            bytes: &[u8],
        ) -> ArtifactResult<OutputRef> {
            let write_count = self.writes.get() + 1;
            self.writes.set(write_count);

            if write_count == self.fail_on {
                return Err(ArtifactError::Io(std::io::Error::other(
                    "simulated write failure",
                )));
            }

            Ok(OutputRef {
                id: OutputRefId::new(),
                uri: format!("file://{scope}/{label}/{}-bytes", bytes.len()),
                media_type: media_type.to_string(),
                label: Some(label.to_string()),
                digest: None,
            })
        }
    }

    #[test]
    fn does_not_mutate_result_when_later_spill_write_fails() {
        let mut result = ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            invocation_id: ToolInvocationId::new(),
            tool_id: "fs.search".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("tool_result.test.v1".to_string()),
            fields: vec![
                ToolResultField {
                    key: "matches".to_string(),
                    value: StructuredValue::String("abcdef".into()),
                },
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String("ghijkl".into()),
                },
            ],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        };

        let expected = result.clone();
        let writer = FailingWriter::new(2);

        let error = spill_large_tool_result_fields_with_writer(
            &mut result,
            &writer,
            "repo_example",
            &ToolResultSpilloverOptions::new(4),
        )
        .unwrap_err();

        assert!(matches!(error, ArtifactError::Io(_)));
        assert_eq!(expected, result);
    }

    #[test]
    fn rolls_back_partial_artifact_writes_on_later_failure() {
        let root = temp_root("spill-rollback");
        let delegate = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let mut result = ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            invocation_id: ToolInvocationId::new(),
            tool_id: "fs.search".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("tool_result.test.v1".to_string()),
            fields: vec![
                ToolResultField {
                    key: "matches".to_string(),
                    value: StructuredValue::String("abcdef".into()),
                },
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String("ghijkl".into()),
                },
            ],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        };
        let expected = result.clone();
        let writer = FailingLocalWriter::new(delegate, 2);

        let error = spill_large_tool_result_fields_with_writer(
            &mut result,
            &writer,
            "repo_example",
            &ToolResultSpilloverOptions::new(4),
        )
        .unwrap_err();

        assert!(matches!(error, ArtifactError::Io(_)));
        assert_eq!(expected, result);

        let scope_dir = root.join("repo_example");
        let entries = if scope_dir.exists() {
            fs::read_dir(&scope_dir)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        } else {
            Vec::new()
        };
        assert!(entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn writes_artifacts_with_restrictive_unix_permissions() {
        let root = temp_root("perm");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        let dir = root.join("repo_example");
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(0o700, dir_mode);

        let file_mode = fs::metadata(&reference.uri).unwrap().permissions().mode() & 0o777;
        assert_eq!(0o600, file_mode);
    }

    #[cfg(unix)]
    #[test]
    fn rewrites_existing_artifact_with_restrictive_unix_mode() {
        let root = temp_root("existing-perm");
        let dir = root.join("repo_example");
        create_scope_dir(&dir).unwrap();
        let file_path = dir.join("existing.artifact");

        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&file_path)
                .unwrap();
            use std::io::Write;
            file.write_all(b"loose").unwrap();
        }
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644)).unwrap();

        let pre_mode = fs::metadata(&file_path).unwrap().permissions().mode() & 0o777;
        assert_ne!(0o600, pre_mode);

        write_file_bytes(&file_path, b"second").unwrap();

        let post_mode = fs::metadata(&file_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(0o600, post_mode);
    }

    #[cfg(unix)]
    #[test]
    fn tightens_existing_scope_directory_permissions_on_write() {
        let root = temp_root("existing-scope");
        let dir = root.join("repo_example");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();

        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(0o700, dir_mode);

        let file_mode = fs::metadata(&reference.uri).unwrap().permissions().mode() & 0o777;
        assert_eq!(0o600, file_mode);
    }
}
