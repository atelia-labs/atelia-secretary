//! Local artifact storage and spillover helpers for large tool outputs.

use crate::domain::{
    LedgerTimestamp, OutputRef, OutputRefId, StructuredValue, ToolResult, ToolResultField,
    TruncationMetadata,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const WRITE_FILE_TMP_PREFIX: &str = ".atelia-artifact-tmp";
static WRITE_FILE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const DEFAULT_ARTIFACT_APP_DIR: &str = "atelia-secretary";
const DEFAULT_ARTIFACT_DIR: &str = "artifacts";
const DEFAULT_ARTIFACT_INDEX_FILE: &str = "index.json";
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactTombstone {
    pub at: LedgerTimestamp,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalArtifactRecord {
    pub id: OutputRefId,
    pub scope: String,
    pub project_id: Option<String>,
    pub repository_id: Option<String>,
    pub path: String,
    pub uri: String,
    pub media_type: String,
    pub label: Option<String>,
    pub created_at: LedgerTimestamp,
    pub original_bytes: Option<u64>,
    pub retained_bytes: Option<u64>,
    pub tombstone: Option<ArtifactTombstone>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactWriteMetadata {
    pub project_id: Option<String>,
    pub repository_id: Option<String>,
    pub original_bytes: Option<u64>,
    pub retained_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRetentionPolicy {
    pub max_retention: Duration,
}

impl ArtifactRetentionPolicy {
    pub fn new(max_retention: Duration) -> Self {
        Self { max_retention }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactExpirationReport {
    pub requested: usize,
    pub matched: usize,
    pub tombstoned: usize,
    pub deleted_files: usize,
    pub missing_files: usize,
}

impl ArtifactExpirationReport {
    fn new(requested: usize) -> Self {
        Self {
            requested,
            matched: 0,
            tombstoned: 0,
            deleted_files: 0,
            missing_files: 0,
        }
    }
}

#[derive(Debug)]
pub enum ArtifactError {
    InvalidScope { scope: String },
    Io(io::Error),
    InvalidIndex { path: String, reason: String },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScope { scope } => write!(f, "invalid artifact scope: {scope}"),
            Self::Io(error) => write!(f, "artifact io failed: {error}"),
            Self::InvalidIndex { path, reason } => {
                write!(f, "artifact index corrupted at {path}: {reason}")
            }
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidScope { .. } | Self::InvalidIndex { .. } => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for ArtifactError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ArtifactError {
    fn from(error: serde_json::Error) -> Self {
        Self::InvalidIndex {
            path: "<serde>".to_string(),
            reason: error.to_string(),
        }
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

    fn update_spill_metadata(
        &self,
        _scope: &str,
        _output_ref: &OutputRef,
        _original_bytes: Option<u64>,
        _retained_bytes: Option<u64>,
    ) -> ArtifactResult<()> {
        Ok(())
    }

    fn delete_artifact_record(&self, _scope: &str, _output_ref: &OutputRef) -> ArtifactResult<()> {
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

    fn update_spill_metadata(
        &self,
        scope: &str,
        output_ref: &OutputRef,
        original_bytes: Option<u64>,
        retained_bytes: Option<u64>,
    ) -> ArtifactResult<()> {
        update_record_bytes(self, scope, &output_ref.id, original_bytes, retained_bytes)
    }

    fn delete_artifact_record(&self, scope: &str, output_ref: &OutputRef) -> ArtifactResult<()> {
        remove_record_bytes(self, scope, &output_ref.id)
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
        self.write_bytes_with_metadata(
            scope,
            label,
            media_type,
            bytes,
            ArtifactWriteMetadata::default(),
        )
    }

    pub fn write_bytes_with_metadata(
        &self,
        scope: &str,
        label: impl AsRef<str>,
        media_type: impl Into<String>,
        bytes: &[u8],
        metadata: ArtifactWriteMetadata,
    ) -> ArtifactResult<OutputRef> {
        let scope_dir_name = sanitize_segment(scope)?;
        let scope_label = label.as_ref();
        let safe_label = sanitize_label(scope_label);
        let id = OutputRefId::new();
        let file_name = format!("{}-{safe_label}.artifact", id.as_str());
        let dir = self.config.root_dir.join(&scope_dir_name);
        let path = dir.join(file_name);

        create_scope_dir(&dir)?;
        write_file_bytes(&path, bytes)?;

        let uri = path.to_string_lossy().into_owned();
        let media_type = media_type.into();
        let created_at = LedgerTimestamp::now();
        let index_record = LocalArtifactRecord {
            id: id.clone(),
            scope: scope_dir_name.clone(),
            project_id: metadata.project_id,
            repository_id: metadata.repository_id,
            path: uri.clone(),
            uri: uri.clone(),
            media_type: media_type.clone(),
            label: Some(scope_label.to_string()),
            created_at,
            original_bytes: metadata.original_bytes,
            retained_bytes: metadata.retained_bytes,
            tombstone: None,
        };

        let output_ref = OutputRef {
            id,
            uri: uri.clone(),
            media_type: media_type.clone(),
            label: Some(scope_label.to_string()),
            digest: None,
        };

        let write_result = (|| {
            let _guard = ScopeIndexGuard::acquire(&dir)?;
            let mut records = self.read_scope_records(&dir)?;
            records.push(index_record);
            self.write_scope_records_unlocked(&dir, records)
        })();

        if let Err(error) = write_result {
            let _ = self.delete_artifact(&output_ref);
            let _ = fs::remove_file(&path);
            return Err(error);
        }

        Ok(output_ref)
    }

    pub fn list_records(&self, scope: Option<&str>) -> ArtifactResult<Vec<LocalArtifactRecord>> {
        let mut records = match scope {
            Some(scope) => {
                let scope_dir = self.scope_dir(scope)?;
                if !scope_dir.exists() {
                    return Ok(Vec::new());
                }
                self.read_scope_records(&scope_dir)?
            }
            None => self.read_all_records()?,
        };

        records.sort_by(|left, right| {
            left.created_at
                .unix_millis
                .cmp(&right.created_at.unix_millis)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });

        Ok(records)
    }

    pub fn find_expired_artifact_records(
        &self,
        scope: Option<&str>,
        now: LedgerTimestamp,
        policy: &ArtifactRetentionPolicy,
    ) -> ArtifactResult<Vec<LocalArtifactRecord>> {
        let retention_cutoff = expired_cutoff_millis(now, policy);
        let mut records = self.list_records(scope)?;

        records.retain(|record| {
            record.created_at.unix_millis <= retention_cutoff
                && (record.tombstone.is_none() || Path::new(&record.path).exists())
        });
        Ok(records)
    }

    pub fn safe_expire_artifact_records(
        &self,
        scope: Option<&str>,
        record_ids: &[OutputRefId],
        now: LedgerTimestamp,
        tombstone_reason: impl Into<String>,
    ) -> ArtifactResult<ArtifactExpirationReport> {
        let records = self.list_records(scope)?;
        let expected_scope = scope.map(sanitize_segment).transpose()?;
        let tombstone_reason = tombstone_reason.into();
        let mut target_ids: HashSet<OutputRefId> = HashSet::new();
        for record_id in record_ids {
            target_ids.insert(record_id.clone());
        }

        let mut report = ArtifactExpirationReport::new(target_ids.len());
        if report.requested == 0 || records.is_empty() {
            return Ok(report);
        }

        let mut scope_targets: BTreeMap<String, HashSet<OutputRefId>> = BTreeMap::new();
        for record in records {
            let sanitized_scope = sanitize_segment(&record.scope)?;
            if expected_scope
                .as_ref()
                .is_some_and(|expected| sanitized_scope != *expected)
            {
                return Err(ArtifactError::InvalidScope {
                    scope: record.scope,
                });
            }
            if sanitized_scope != record.scope {
                return Err(ArtifactError::InvalidScope {
                    scope: record.scope,
                });
            }

            if target_ids.contains(&record.id) {
                report.matched += 1;
                scope_targets
                    .entry(sanitized_scope)
                    .or_default()
                    .insert(record.id);
            }
        }

        for (scope, target_ids) in scope_targets {
            let scope_dir = self.config.root_dir.join(&scope);
            let mut deletions = Vec::new();

            {
                let _guard = ScopeIndexGuard::acquire(&scope_dir)?;
                let mut scope_records = self.read_scope_records(&scope_dir)?;
                for record in &mut scope_records {
                    if !target_ids.contains(&record.id) {
                        continue;
                    }

                    if record.tombstone.is_none() {
                        report.tombstoned += 1;
                        record.tombstone = Some(ArtifactTombstone {
                            at: now,
                            reason: tombstone_reason.clone(),
                        });
                    }

                    if validate_record_path(&self.config.root_dir, &scope, &record.id, &record.path)
                    {
                        let output_ref = OutputRef {
                            id: record.id.clone(),
                            uri: record.path.clone(),
                            media_type: record.media_type.clone(),
                            label: record.label.clone(),
                            digest: None,
                        };
                        deletions.push((output_ref, Path::new(&record.path).to_path_buf()));
                    }
                }

                self.write_scope_records_unlocked(&scope_dir, scope_records)?;
            }

            for (output_ref, path) in deletions.drain(..) {
                let existed = path.exists();
                self.delete_artifact(&output_ref)?;
                let still_exists = path.exists();

                if existed && !still_exists {
                    report.deleted_files += 1;
                } else if !existed && !still_exists {
                    report.missing_files += 1;
                }
            }
        }

        Ok(report)
    }

    pub fn safe_expire_artifacts_by_retention(
        &self,
        scope: Option<&str>,
        now: LedgerTimestamp,
        policy: &ArtifactRetentionPolicy,
        tombstone_reason: impl Into<String>,
    ) -> ArtifactResult<ArtifactExpirationReport> {
        let expired_ids = self
            .find_expired_artifact_records(scope, now, policy)?
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        self.safe_expire_artifact_records(scope, &expired_ids, now, tombstone_reason)
    }

    fn scope_dir(&self, scope: &str) -> ArtifactResult<PathBuf> {
        Ok(self.config.root_dir.join(sanitize_segment(scope)?))
    }

    fn read_all_records(&self) -> ArtifactResult<Vec<LocalArtifactRecord>> {
        if !self.config.root_dir.exists() {
            return Ok(Vec::new());
        }

        let mut scope_dirs: Vec<PathBuf> = fs::read_dir(&self.config.root_dir)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|entry| {
                let path = entry.path();
                let is_dir = entry
                    .file_type()
                    .ok()
                    .is_some_and(|file_type| file_type.is_dir());
                if is_dir {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        scope_dirs.sort();
        let mut records = Vec::new();
        for scope_dir in scope_dirs {
            let source_scope_name = scope_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let source_scope = sanitize_segment(source_scope_name)?;
            if source_scope != source_scope_name {
                return Err(ArtifactError::InvalidScope {
                    scope: source_scope_name.to_string(),
                });
            }

            let mut scoped_records = self.read_scope_records(&scope_dir)?;
            for record in &mut scoped_records {
                let record_scope = sanitize_segment(&record.scope)?;
                if record.scope != record_scope {
                    return Err(ArtifactError::InvalidScope {
                        scope: record.scope.clone(),
                    });
                }
                if record_scope != source_scope {
                    return Err(ArtifactError::InvalidScope {
                        scope: record.scope.clone(),
                    });
                }
                record.scope = source_scope.clone();
            }

            records.extend(scoped_records);
        }

        Ok(records)
    }

    fn read_scope_records(&self, scope_dir: &Path) -> ArtifactResult<Vec<LocalArtifactRecord>> {
        let index_path = scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE);
        if !index_path.exists() {
            return Ok(Vec::new());
        }

        let index_contents = fs::read_to_string(&index_path)?;
        if index_contents.trim().is_empty() {
            return Ok(Vec::new());
        }

        serde_json::from_str::<Vec<LocalArtifactRecord>>(&index_contents).map_err(|error| {
            ArtifactError::InvalidIndex {
                path: index_path.to_string_lossy().into_owned(),
                reason: error.to_string(),
            }
        })
    }

    #[cfg(test)]
    fn write_scope_records(
        &self,
        scope_dir: &Path,
        records: Vec<LocalArtifactRecord>,
    ) -> ArtifactResult<()> {
        if !scope_dir.exists() && records.is_empty() {
            return Ok(());
        }

        let _guard = ScopeIndexGuard::acquire(scope_dir)?;
        self.write_scope_records_unlocked(scope_dir, records)
    }

    fn write_scope_records_unlocked(
        &self,
        scope_dir: &Path,
        mut records: Vec<LocalArtifactRecord>,
    ) -> ArtifactResult<()> {
        let index_path = scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE);
        if records.is_empty() {
            match fs::remove_file(&index_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            return Ok(());
        }

        records.sort_by(|left, right| {
            left.created_at
                .unix_millis
                .cmp(&right.created_at.unix_millis)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        let serialized = serde_json::to_vec_pretty(&records)?;
        let counter = WRITE_FILE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = scope_dir.join(format!(".index-tmp-{counter}-{}", std::process::id()));
        if let Err(error) = fs::write(&tmp, &serialized) {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
        if let Err(error) = rename_atomic(&tmp, &index_path) {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
        Ok(())
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

struct ScopeIndexGuard {
    #[cfg(unix)]
    _file: fs::File,
    #[cfg(not(unix))]
    lock_path: PathBuf,
}

impl ScopeIndexGuard {
    fn acquire(scope_dir: &Path) -> io::Result<Self> {
        let lock_path = scope_dir.join(".index.lock");

        #[cfg(unix)]
        {
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)?;
            let fd = file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "failed to acquire scope index lock at {}: lock is held",
                        lock_path.display()
                    ),
                ));
            }
            Ok(Self { _file: file })
        }

        #[cfg(not(unix))]
        {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => Ok(Self { lock_path }),
                Err(e) => Err(io::Error::new(
                    e.kind(),
                    format!(
                        "failed to acquire scope index lock at {}: {e}",
                        lock_path.display()
                    ),
                )),
            }
        }
    }
}

#[cfg(not(unix))]
impl Drop for ScopeIndexGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
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
                rollback_artifact_writes(store, scope, &written_output_refs);
                return Err(error);
            }
        };
        let replacement = format!("artifact_ref {}", output_ref.uri);
        let retained_size = replacement.len() as u64;
        let original_size = bytes.len() as u64;
        if let Err(error) = store.update_spill_metadata(
            scope,
            &output_ref,
            Some(original_size),
            Some(retained_size),
        ) {
            rollback_artifact_writes(store, scope, &written_output_refs);
            return Err(error);
        }

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

fn rollback_artifact_writes(writer: &impl ArtifactWriter, scope: &str, output_refs: &[OutputRef]) {
    for output_ref in output_refs {
        let _ = writer.delete_artifact_bytes(output_ref);
        let _ = writer.delete_artifact_record(scope, output_ref);
    }
}

fn update_record_bytes(
    store: &LocalArtifactStore,
    scope: &str,
    id: &OutputRefId,
    original_bytes: Option<u64>,
    retained_bytes: Option<u64>,
) -> ArtifactResult<()> {
    let scope_dir = store.config.root_dir.join(sanitize_segment(scope)?);
    let _guard = ScopeIndexGuard::acquire(&scope_dir)?;
    let mut records = store.read_scope_records(&scope_dir)?;
    let mut needs_persist = false;

    for record in &mut records {
        if record.id == *id {
            if let Some(original_bytes) = original_bytes {
                record.original_bytes = Some(original_bytes);
                needs_persist = true;
            }
            if let Some(retained_bytes) = retained_bytes {
                record.retained_bytes = Some(retained_bytes);
                needs_persist = true;
            }
            break;
        }
    }

    if !needs_persist {
        return Ok(());
    }

    store.write_scope_records_unlocked(&scope_dir, records)?;
    Ok(())
}

fn remove_record_bytes(
    store: &LocalArtifactStore,
    scope: &str,
    id: &OutputRefId,
) -> ArtifactResult<()> {
    let scope_dir = store.config.root_dir.join(sanitize_segment(scope)?);
    let _guard = ScopeIndexGuard::acquire(&scope_dir)?;
    let mut records = store.read_scope_records(&scope_dir)?;
    records.retain(|record| record.id != *id);
    store.write_scope_records_unlocked(&scope_dir, records)?;
    Ok(())
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

fn expired_cutoff_millis(now: LedgerTimestamp, policy: &ArtifactRetentionPolicy) -> i64 {
    let retention_millis = i64::try_from(policy.max_retention.as_millis()).unwrap_or(i64::MAX);
    now.unix_millis.saturating_sub(retention_millis)
}

fn validate_record_path(
    root_dir: &Path,
    sanitized_scope: &str,
    record_id: &OutputRefId,
    record_path: &str,
) -> bool {
    let path = Path::new(record_path);

    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };

    if file_name == DEFAULT_ARTIFACT_INDEX_FILE {
        return false;
    }

    if !file_name.starts_with(record_id.as_str()) {
        return false;
    }

    let expected_scope_dir = root_dir.join(sanitized_scope);
    let canonical_scope_dir = match expected_scope_dir.canonicalize() {
        Ok(dir) => dir,
        Err(_) => return false,
    };
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    canonical_path.starts_with(&canonical_scope_dir)
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
    #[cfg(unix)]
    use std::os::unix::io::AsRawFd;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("atelia-artifacts-{name}-{unique}"))
    }

    #[cfg(unix)]
    fn hold_index_lock(scope_dir: &Path) -> fs::File {
        let lock_path = scope_dir.join(".index.lock");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) },
            0,
            "failed to hold test lock"
        );
        file
    }

    #[cfg(not(unix))]
    fn hold_index_lock(scope_dir: &Path) -> bool {
        let lock_path = scope_dir.join(".index.lock");
        fs::write(&lock_path, b"").unwrap();
        true
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
    fn writes_artifact_index_metadata() {
        let root = temp_root("index");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));

        let reference = store
            .write_bytes_with_metadata(
                "repo_example",
                "search output",
                "text/plain",
                b"hello",
                ArtifactWriteMetadata {
                    project_id: Some("project-1".to_string()),
                    repository_id: Some("repo-a".to_string()),
                    original_bytes: Some(5),
                    retained_bytes: Some(7),
                },
            )
            .unwrap();

        let records = store.list_records(None).unwrap();
        assert_eq!(1, records.len());

        let record = &records[0];
        assert_eq!(reference.id, record.id);
        assert_eq!("repo_example", record.scope.as_str());
        assert_eq!(Some("project-1".to_string()), record.project_id);
        assert_eq!(Some("repo-a".to_string()), record.repository_id);
        assert_eq!(reference.uri, record.path);
        assert_eq!(reference.uri, record.uri);
        assert_eq!("text/plain", record.media_type);
        assert_eq!(Some("search output".to_string()), record.label);
        assert_eq!(Some(5), record.original_bytes);
        assert_eq!(Some(7), record.retained_bytes);
        assert_eq!(None, record.tombstone);
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
        assert_eq!(6, report.original_bytes);
        assert_eq!(
            result.output_refs[0].uri.len() as u64 + 13,
            report.retained_bytes
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
    fn finds_expired_artifact_records_without_deleting() {
        let root = temp_root("expire-find");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let expired = store
            .find_expired_artifact_records(None, LedgerTimestamp::now(), &policy)
            .unwrap();

        assert_eq!(1, expired.len());
        assert_eq!(reference.id, expired[0].id);
        let records = store.list_records(None).unwrap();
        assert_eq!(None, records[0].tombstone);
        assert!(Path::new(&reference.uri).exists());
    }

    #[test]
    fn safe_expire_artifact_records_keeps_tombstone_metadata() {
        let root = temp_root("expire-safe");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let report = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap();

        assert_eq!(1, report.requested);
        assert_eq!(1, report.matched);
        assert_eq!(1, report.tombstoned);
        assert_eq!(1, report.deleted_files);

        let records = store.list_records(None).unwrap();
        assert_eq!(
            Some("retention policy"),
            records[0].tombstone.as_ref().map(|t| t.reason.as_str())
        );
        assert_eq!(reference.id, records[0].id);
        assert!(!Path::new(&reference.uri).exists());
    }

    #[test]
    fn safe_expire_artifact_records_skips_records_with_mismatched_filename() {
        let root = temp_root("expire-bad-filename");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();

        let mismatched_file = scope_dir.join("mismatched.artifact");
        fs::write(&mismatched_file, b"stale").unwrap();
        let record_id = OutputRefId::new();
        let record = LocalArtifactRecord {
            id: record_id.clone(),
            scope: "repo_example".to_string(),
            project_id: None,
            repository_id: None,
            path: mismatched_file.to_string_lossy().into_owned(),
            uri: mismatched_file.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("mismatched".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };
        store.write_scope_records(&scope_dir, vec![record]).unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let report = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap();

        assert_eq!(1, report.requested);
        assert_eq!(1, report.matched);
        assert_eq!(1, report.tombstoned);
        assert_eq!(0, report.deleted_files);
        assert!(
            mismatched_file.exists(),
            "file with mismatched name must not be deleted"
        );

        let records = store.list_records(None).unwrap();
        assert_eq!(1, records.len());
        assert_eq!(
            Some("retention policy"),
            records[0].tombstone.as_ref().map(|t| t.reason.as_str())
        );
    }

    #[test]
    fn safe_expire_artifact_records_skips_paths_outside_root() {
        let root = temp_root("expire-outside-root");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let outside_root = temp_root("expire-outside-root-target");
        let outside_scope = outside_root.join("other_scope");
        create_scope_dir(&outside_scope).unwrap();

        let outside_file = outside_scope.join("outside.artifact");
        fs::write(&outside_file, b"don't touch").unwrap();

        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();
        let forged = LocalArtifactRecord {
            id: OutputRefId::new(),
            scope: "repo_example".to_string(),
            project_id: None,
            repository_id: None,
            path: outside_file.to_string_lossy().into_owned(),
            uri: outside_file.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };

        store.write_scope_records(&scope_dir, vec![forged]).unwrap();
        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let record_id = store.list_records(None).unwrap()[0].id.clone();
        let report = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap();

        assert_eq!(1, report.requested);
        assert_eq!(1, report.matched);
        assert_eq!(1, report.tombstoned);
        assert_eq!(0, report.deleted_files);
        assert!(outside_file.exists());
        let records = store.list_records(None).unwrap();
        assert_eq!(1, records.len());
        assert_eq!(
            Some("retention policy"),
            records[0].tombstone.as_ref().map(|t| t.reason.as_str())
        );
        assert_eq!(record_id, records[0].id);
    }

    #[test]
    fn safe_expire_artifact_records_rejects_forged_scope_traversal() {
        let root = temp_root("expire-fake-scope");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let escaped_root = temp_root("expire-fake-scope-target");
        create_scope_dir(&escaped_root).unwrap();

        let escaped_index = escaped_root.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let escaped_index_before = b"do-not-touch".to_vec();
        fs::write(&escaped_index, &escaped_index_before).unwrap();

        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();
        let scoped_artifact = scope_dir.join("artifact.artifact");
        fs::write(&scoped_artifact, b"stale").unwrap();

        let escaped_scope = format!(
            "..{}{}",
            std::path::MAIN_SEPARATOR,
            escaped_root
                .file_name()
                .expect("temp root should have file name")
                .to_string_lossy()
        );
        let forged = LocalArtifactRecord {
            id: OutputRefId::new(),
            scope: escaped_scope.clone(),
            project_id: None,
            repository_id: None,
            path: scoped_artifact.to_string_lossy().into_owned(),
            uri: scoped_artifact.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };

        store.write_scope_records(&scope_dir, vec![forged]).unwrap();
        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::InvalidScope {
                scope
            } if scope == escaped_scope
        ));
        assert_eq!(
            escaped_index_before,
            fs::read(&escaped_index).unwrap(),
            "forged index outside root must not be rewritten"
        );
        assert!(!escaped_root.join("index.json.tmp").exists());
        assert!(
            !escaped_root.join(".index.lock").exists(),
            "no stale lock files in escaped root"
        );
        assert!(Path::new(&scoped_artifact).exists());
        let records = store.list_records(Some("repo_example")).unwrap();
        assert_eq!(1, records.len());
        assert_eq!(None, records[0].tombstone);
        assert_eq!(records[0].scope, escaped_scope);
    }

    #[test]
    fn safe_expire_artifact_records_rejects_forged_in_scope_record_scope() {
        let root = temp_root("expire-fake-scope-inside-root");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_a = root.join("scope_a");
        let scope_b = root.join("scope_b");
        create_scope_dir(&scope_a).unwrap();
        create_scope_dir(&scope_b).unwrap();

        let scope_b_index = scope_b.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let scope_b_index_before = b"do-not-touch".to_vec();
        fs::write(&scope_b_index, &scope_b_index_before).unwrap();

        let reference = store
            .write_bytes("scope_a", "search output", "text/plain", b"stale")
            .unwrap();

        let mut forged_record = store
            .list_records(Some("scope_a"))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        forged_record.scope = "scope_b".to_string();
        store
            .write_scope_records(&scope_a, vec![forged_record])
            .unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::InvalidScope {
                scope
            } if scope == "scope_b"
        ));
        assert!(Path::new(&scope_b_index).exists());
        assert_eq!(
            scope_b_index_before,
            fs::read(&scope_b_index).unwrap(),
            "scope_b index must not be rewritten"
        );
        assert!(
            Path::new(&reference.uri).exists(),
            "forged artifact path inside root must not be deleted"
        );

        let records = store.list_records(Some("scope_a")).unwrap();
        assert_eq!(1, records.len());
        assert_eq!("scope_b", records[0].scope);
    }

    #[test]
    fn safe_expire_artifacts_by_retention_rejects_forged_in_scope_record_scope() {
        let root = temp_root("expire-fake-scope-inside-root-scoped");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_a = root.join("scope_a");
        let scope_b = root.join("scope_b");
        create_scope_dir(&scope_a).unwrap();
        create_scope_dir(&scope_b).unwrap();

        let scope_a_index = scope_a.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let scope_b_index = scope_b.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let scope_b_index_before = b"do-not-touch".to_vec();
        fs::write(&scope_b_index, &scope_b_index_before).unwrap();

        let reference = store
            .write_bytes("scope_a", "search output", "text/plain", b"stale")
            .unwrap();

        let mut forged_record = store
            .list_records(Some("scope_a"))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        forged_record.scope = "scope_b".to_string();
        store
            .write_scope_records(&scope_a, vec![forged_record])
            .unwrap();
        let scope_a_index_before = fs::read(&scope_a_index).unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                Some("scope_a"),
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::InvalidScope {
                scope
            } if scope == "scope_b"
        ));
        assert!(Path::new(&scope_a_index).exists());
        assert!(Path::new(&scope_b_index).exists());
        assert_eq!(
            scope_a_index_before,
            fs::read(&scope_a_index).unwrap(),
            "scope_a index must not be rewritten"
        );
        assert_eq!(
            scope_b_index_before,
            fs::read(&scope_b_index).unwrap(),
            "scope_b index must not be rewritten"
        );
        assert!(
            Path::new(&reference.uri).exists(),
            "forged artifact path inside root must not be deleted"
        );
    }

    #[test]
    fn safe_expire_artifacts_by_retention_rejects_non_canonical_record_scope_within_scope_dir() {
        let root = temp_root("expire-fake-record-scope-unscoped");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));

        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();
        let artifact_path = scope_dir.join("artifact.artifact");
        fs::write(&artifact_path, b"stale").unwrap();

        let forged_record = LocalArtifactRecord {
            id: OutputRefId::new(),
            scope: "repo example".to_string(),
            project_id: None,
            repository_id: None,
            path: artifact_path.to_string_lossy().into_owned(),
            uri: artifact_path.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };
        store
            .write_scope_records(&scope_dir, vec![forged_record])
            .unwrap();

        let index_path = scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let index_before = fs::read(&index_path).unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::InvalidScope { scope } if scope == "repo example"
        ));
        assert_eq!(index_before, fs::read(&index_path).unwrap());
        assert!(artifact_path.exists());

        let records = store.list_records(Some("repo_example")).unwrap();
        assert_eq!(1, records.len());
        assert_eq!("repo example", records[0].scope);
        assert_eq!(None, records[0].tombstone);
    }

    #[test]
    fn safe_expire_artifact_records_rejects_non_canonical_source_directory() {
        let root = temp_root("expire-fake-non-canonical-source-dir");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));

        let canonical_scope = "repo_example";
        let reference = store
            .write_bytes(canonical_scope, "search output", "text/plain", b"kept")
            .unwrap();
        let canonical_scope_dir = root.join(canonical_scope);
        let canonical_index = canonical_scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let canonical_index_before = fs::read(&canonical_index).unwrap();

        let forged_dir_name = "repo example";
        let forged_scope_dir = root.join(forged_dir_name);
        create_scope_dir(&forged_scope_dir).unwrap();
        let forged_artifact = forged_scope_dir.join("forged.artifact");
        fs::write(&forged_artifact, b"forged").unwrap();
        let forged_record = LocalArtifactRecord {
            id: OutputRefId::new(),
            scope: canonical_scope.to_string(),
            project_id: None,
            repository_id: None,
            path: forged_artifact.to_string_lossy().into_owned(),
            uri: forged_artifact.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };
        store
            .write_scope_records(&forged_scope_dir, vec![forged_record])
            .unwrap();
        let forged_index = forged_scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE);
        let forged_index_before = fs::read(&forged_index).unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::InvalidScope { scope }
            if scope == forged_dir_name
        ));
        assert_eq!(
            canonical_index_before,
            fs::read(&canonical_index).unwrap(),
            "canonical index must not be rewritten"
        );
        assert_eq!(
            forged_index_before,
            fs::read(&forged_index).unwrap(),
            "forged index must not be rewritten"
        );
        assert!(Path::new(&reference.uri).exists());
        assert!(Path::new(&forged_artifact).exists());
    }

    #[test]
    fn safe_expire_artifact_records_tombstones_before_delete_on_index_failure() {
        let root = temp_root("expire-rollback");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let reference = store
            .write_bytes("repo_example", "search output", "text/plain", b"hello")
            .unwrap();

        let scope_dir = root.join("repo_example");
        let _held_lock = hold_index_lock(&scope_dir);

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::Io(_)));
        assert!(Path::new(&reference.uri).exists());
    }

    #[test]
    fn safe_expire_artifact_records_retries_tombstoned_record_when_directly_targeted() {
        let root = temp_root("expire-retry-tombstone");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();

        let record_id = OutputRefId::new();
        let forged_path = scope_dir.join(format!("{}-forged", record_id.as_str()));
        fs::create_dir(&forged_path).unwrap();
        let record = LocalArtifactRecord {
            id: record_id.clone(),
            scope: "repo_example".to_string(),
            project_id: None,
            repository_id: None,
            path: forged_path.to_string_lossy().into_owned(),
            uri: forged_path.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };
        store.write_scope_records(&scope_dir, vec![record]).unwrap();

        let error = store
            .safe_expire_artifact_records(
                None,
                std::slice::from_ref(&record_id),
                LedgerTimestamp::now(),
                "retention policy",
            )
            .unwrap_err();
        assert!(matches!(error, ArtifactError::Io(_)));

        let records = store.list_records(None).unwrap();
        assert_eq!(1, records.len());
        assert_eq!(
            Some("retention policy"),
            records[0]
                .tombstone
                .as_ref()
                .map(|tombstone| tombstone.reason.as_str())
        );
        assert!(forged_path.exists());

        let retry_path = scope_dir.join(format!("{}.artifact", record_id.as_str()));
        fs::write(&retry_path, b"retryable").unwrap();

        let mut retriable = records[0].clone();
        retriable.path = retry_path.to_string_lossy().into_owned();
        retriable.uri = retriable.path.clone();
        store
            .write_scope_records(&scope_dir, vec![retriable])
            .unwrap();

        let report = store
            .safe_expire_artifact_records(
                None,
                std::slice::from_ref(&record_id),
                LedgerTimestamp::now(),
                "retry",
            )
            .unwrap();

        assert_eq!(1, report.requested);
        assert_eq!(1, report.matched);
        assert_eq!(0, report.tombstoned);
        assert_eq!(1, report.deleted_files);
        assert!(!retry_path.exists());
    }

    #[test]
    fn write_bytes_with_metadata_cleans_up_when_scope_lock_is_held() {
        let root = temp_root("write-lock-held");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();
        let _held_lock = hold_index_lock(&scope_dir);

        let error = store
            .write_bytes_with_metadata(
                "repo_example",
                "search output",
                "text/plain",
                b"hello",
                ArtifactWriteMetadata::default(),
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::Io(error) if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists
        )));
        let remaining_entries: Vec<_> = fs::read_dir(&scope_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(1, remaining_entries.len());
        assert_eq!(".index.lock", remaining_entries[0].to_string_lossy());
    }

    #[test]
    fn safe_expire_artifacts_by_retention_retries_tombstoned_record_when_still_present() {
        let root = temp_root("expire-retry-tombstone-retention");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();

        let record_id = OutputRefId::new();
        let forged_path = scope_dir.join(format!("{}-forged", record_id.as_str()));
        fs::create_dir(&forged_path).unwrap();
        let record = LocalArtifactRecord {
            id: record_id.clone(),
            scope: "repo_example".to_string(),
            project_id: None,
            repository_id: None,
            path: forged_path.to_string_lossy().into_owned(),
            uri: forged_path.to_string_lossy().into_owned(),
            media_type: "text/plain".to_string(),
            label: Some("forged".to_string()),
            created_at: LedgerTimestamp::now(),
            original_bytes: None,
            retained_bytes: None,
            tombstone: None,
        };
        store.write_scope_records(&scope_dir, vec![record]).unwrap();

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();
        assert!(matches!(error, ArtifactError::Io(_)));

        let records = store.list_records(None).unwrap();
        assert_eq!(1, records.len());
        assert_eq!(
            Some("retention policy"),
            records[0]
                .tombstone
                .as_ref()
                .map(|tombstone| tombstone.reason.as_str())
        );
        assert!(forged_path.exists());

        let retry_path = scope_dir.join(format!("{}.artifact", record_id.as_str()));
        fs::write(&retry_path, b"retryable").unwrap();

        let mut retriable = records[0].clone();
        retriable.path = retry_path.to_string_lossy().into_owned();
        retriable.uri = retriable.path.clone();
        store
            .write_scope_records(&scope_dir, vec![retriable])
            .unwrap();

        let report = store
            .safe_expire_artifacts_by_retention(None, LedgerTimestamp::now(), &policy, "retry")
            .unwrap();

        assert_eq!(1, report.requested);
        assert_eq!(1, report.matched);
        assert_eq!(0, report.tombstoned);
        assert_eq!(1, report.deleted_files);
        assert_eq!(0, report.missing_files);
        assert_eq!(
            Some("retention policy"),
            store.list_records(None).unwrap()[0]
                .tombstone
                .as_ref()
                .map(|tombstone| tombstone.reason.as_str())
        );
        assert!(!retry_path.exists());
    }

    #[test]
    fn safe_expire_artifact_records_tombstones_for_earlier_scopes_are_not_orphaned_on_later_failure(
    ) {
        let root = temp_root("expire-multi-scope-rollback");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let retained_ref = store
            .write_bytes("scope_a", "search output", "text/plain", b"retained")
            .unwrap();
        let failed_ref = store
            .write_bytes("scope_z", "search output", "text/plain", b"failed")
            .unwrap();

        let failing_scope_dir = root.join("scope_z");
        let _held_lock = hold_index_lock(&failing_scope_dir);

        let policy = ArtifactRetentionPolicy::new(Duration::from_millis(0));
        let error = store
            .safe_expire_artifacts_by_retention(
                None,
                LedgerTimestamp::now(),
                &policy,
                "retention policy",
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::Io(_)));
        assert!(!Path::new(&retained_ref.uri).exists());
        assert!(Path::new(&failed_ref.uri).exists());

        let records = store.list_records(None).unwrap();
        let retained_record = records
            .iter()
            .find(|record| record.id == retained_ref.id)
            .expect("retained scope record should remain indexed");
        let failed_record = records
            .iter()
            .find(|record| record.id == failed_ref.id)
            .expect("failed scope record should remain indexed");

        assert_eq!(
            Some("retention policy"),
            retained_record
                .tombstone
                .as_ref()
                .map(|t| t.reason.as_str())
        );
        assert_eq!(None, failed_record.tombstone);
    }

    #[test]
    fn write_bytes_with_metadata_rolls_back_on_index_write_failure() {
        let root = temp_root("write-index-rollback");
        let store = LocalArtifactStore::new(ArtifactStoreConfig::new(&root));
        let scope_dir = root.join("repo_example");
        create_scope_dir(&scope_dir).unwrap();
        fs::write(scope_dir.join(DEFAULT_ARTIFACT_INDEX_FILE), b"not-json").unwrap();

        let error = store
            .write_bytes_with_metadata(
                "repo_example",
                "search output",
                "text/plain",
                b"hello",
                ArtifactWriteMetadata::default(),
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::InvalidIndex { .. }));
        let has_artifact_file = fs::read_dir(&scope_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".artifact"));
        assert!(
            !has_artifact_file,
            "remaining artifact entries after rollback: {:?}",
            fs::read_dir(&scope_dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>()
        );
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

        fn delete_artifact_record(
            &self,
            scope: &str,
            output_ref: &OutputRef,
        ) -> ArtifactResult<()> {
            self.delegate.delete_artifact_record(scope, output_ref)
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
        assert!(entries
            .iter()
            .all(|entry| entry.file_name().to_string_lossy() == ".index.lock"));
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
