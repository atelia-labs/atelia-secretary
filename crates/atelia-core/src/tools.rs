//! Built-in filesystem tools for Atelia Secretary.
//!
//! Provides repository-scoped `fs.list`, `fs.stat`, `fs.read`, `fs.search`,
//! `fs.write`, and `fs.patch` tools that implement
//! [`crate::runtime::RuntimeTool`] and enforce path canonicalization with
//! symlink escape rejection per `docs/execution-semantics.md`.

use crate::artifacts::{ArtifactLookupDenyReason, ArtifactLookupResult, LocalArtifactStore};
use crate::domain::{
    LedgerTimestamp, OutputRefId, RedactionMarker, ResolvedPath, StructuredValue, ToolInvocation,
    ToolResult, ToolResultField, ToolResultId, ToolResultStatus, TruncationMetadata,
};
use crate::runtime::RuntimeJobRequest;
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

const TOOLS_SCHEMA_VERSION: u32 = 1;
const DEFAULT_READ_MAX_LINES: usize = 120;
const DEFAULT_READ_MAX_CHARS: usize = 32 * 1024;
const DEFAULT_READ_MAX_SCAN_BYTES: u64 = 1024 * 1024;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 100;
const DEFAULT_SEARCH_MAX_FILE_BYTES: u64 = 64 * 1024;
const DEFAULT_WRITE_MAX_BYTES: usize = 32 * 1024;
#[cfg(unix)]
const WRITE_FILE_TMP_PREFIX: &str = "atelia-write-tmp";
#[cfg(unix)]
static WRITE_FILE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Path canonicalization and scope validation
// ---------------------------------------------------------------------------

/// A successfully resolved path within a repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPath {
    pub canonical: PathBuf,
    pub root: PathBuf,
}

impl CanonicalPath {
    /// The resolved path relative to the repository root.
    pub fn relative_to_root(&self) -> PathBuf {
        self.canonical
            .strip_prefix(&self.root)
            .unwrap_or(&self.canonical)
            .to_path_buf()
    }

    /// A display-safe relative path (root prefix stripped).
    pub fn display_path(&self) -> String {
        let relative = self.relative_to_root();
        if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.to_string_lossy().to_string()
        }
    }
}

/// Errors produced by [`canonicalize_within_scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathResolutionError {
    RootNotFound,
    TargetNotFound { requested: PathBuf },
    OutsideRepositoryScope { resolved: PathBuf, root: PathBuf },
}

impl std::fmt::Display for PathResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootNotFound => write!(f, "repository root not found"),
            Self::TargetNotFound { requested } => {
                write!(f, "target path not found: {}", requested.display())
            }
            Self::OutsideRepositoryScope { resolved, root } => write!(
                f,
                "resolved path {} is outside repository root {}",
                resolved.display(),
                root.display()
            ),
        }
    }
}

impl std::error::Error for PathResolutionError {}

/// Canonicalize a requested path against a repository root.
///
/// Implements the authoritative path algorithm from `docs/execution-semantics.md`:
/// 1. Canonicalize the repository root to an absolute path.
/// 2. Join the requested relative path with the canonical root and canonicalize
///    the result (resolving all symlinks).
/// 3. Reject unless the resolved canonical path starts with the canonical root.
///
/// Symlink escapes are rejected deterministically: any symlink component that
/// resolves outside the repository root causes the canonical path to fall
/// outside the root prefix, triggering rejection.
pub fn canonicalize_within_scope(
    repo_root: &Path,
    relative_path: &Path,
) -> Result<CanonicalPath, PathResolutionError> {
    let canonical_root = repo_root
        .canonicalize()
        .map_err(|_| PathResolutionError::RootNotFound)?;

    let target = if relative_path.is_absolute() {
        relative_path.to_path_buf()
    } else {
        canonical_root.join(relative_path)
    };

    let canonical_target =
        target
            .canonicalize()
            .map_err(|_| PathResolutionError::TargetNotFound {
                requested: target.clone(),
            })?;

    if !canonical_target.starts_with(&canonical_root) {
        return Err(PathResolutionError::OutsideRepositoryScope {
            resolved: canonical_target,
            root: canonical_root,
        });
    }

    Ok(CanonicalPath {
        canonical: canonical_target,
        root: canonical_root,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn target_from_request(request: &RuntimeJobRequest) -> PathBuf {
    PathBuf::from(&request.resource_scope.value)
}

fn resolved_path_for_request(
    repository_root: &Path,
    request: &RuntimeJobRequest,
) -> Vec<ResolvedPath> {
    match canonicalize_within_scope(repository_root, &target_from_request(request)) {
        Ok(canonical) => vec![ResolvedPath {
            requested_path: request.resource_scope.value.clone(),
            resolved_path: canonical.canonical.to_string_lossy().to_string(),
            display_path: canonical.display_path(),
        }],
        Err(_) => Vec::new(),
    }
}

fn make_tool_result(
    invocation: &ToolInvocation,
    status: ToolResultStatus,
    schema_ref: &str,
    fields: Vec<ToolResultField>,
    truncation: Option<TruncationMetadata>,
    redactions: Vec<RedactionMarker>,
) -> ToolResult {
    ToolResult {
        id: ToolResultId::new(),
        schema_version: TOOLS_SCHEMA_VERSION,
        created_at: LedgerTimestamp::now(),
        invocation_id: invocation.id.clone(),
        tool_id: invocation.tool_id.clone(),
        status,
        schema_ref: Some(schema_ref.to_string()),
        fields,
        evidence_refs: Vec::new(),
        output_refs: Vec::new(),
        truncation,
        redactions,
    }
}

fn failed_result(
    invocation: &ToolInvocation,
    schema_ref: &str,
    summary: String,
    error_detail: String,
) -> ToolResult {
    make_tool_result(
        invocation,
        ToolResultStatus::Failed,
        schema_ref,
        vec![
            ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String(summary),
            },
            ToolResultField {
                key: "error".to_string(),
                value: StructuredValue::String(error_detail),
            },
        ],
        None,
        Vec::new(),
    )
}

fn open_canonical_file_within_scope(canonical: &CanonicalPath) -> io::Result<File> {
    let file = open_file_no_follow(&canonical.canonical)?;

    #[cfg(target_os = "linux")]
    {
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let opened_path = fs::canonicalize(fd_path)?;
        if !opened_path.starts_with(&canonical.root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened file escaped repository root",
            ));
        }
    }

    Ok(file)
}

fn open_artifact_file_within_scope(path: &Path) -> io::Result<File> {
    let expected_metadata = fs::metadata(path)?;
    let expected_path = path.canonicalize()?;
    #[cfg(unix)]
    let (expected_dev, expected_ino) = (expected_metadata.dev(), expected_metadata.ino());

    let file = open_file_no_follow(path)?;
    let opened_metadata = file.metadata()?;

    #[cfg(unix)]
    {
        if opened_metadata.dev() != expected_dev || opened_metadata.ino() != expected_ino {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened artifact file did not match resolved record",
            ));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let opened_path = fs::canonicalize(fd_path)?;
        if opened_path != expected_path {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened artifact file path changed after record resolution",
            ));
        }
    }

    Ok(file)
}

#[cfg(unix)]
fn open_file_no_follow(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_file_no_follow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "artifact file reads are best-effort symlink-blocked on non-Unix platforms",
        ));
    }

    fs::File::open(path)
}

#[cfg(unix)]
fn open_parent_no_follow(path: &Path) -> io::Result<File> {
    use std::path::Component;

    let mut dir = if path.is_absolute() {
        File::open("/")?
    } else {
        File::open(".")?
    };

    for component in path.components() {
        let segment = match component {
            Component::RootDir => continue,
            Component::CurDir => continue,
            Component::Normal(segment) => segment,
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported path component for write operations",
                ));
            }
        };

        let cstring = CString::new(segment.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;

        // SAFETY: `dir` is a live directory file descriptor and the `cstring` is valid
        // and nul-terminated for the `openat` syscall.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                cstring.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `fd` was returned from `openat` and is uniquely owned by this branch.
        dir = unsafe { File::from_raw_fd(fd) };
    }

    Ok(dir)
}

#[cfg(unix)]
fn open_no_follow_in_parent_dir(
    parent: &File,
    name: &std::ffi::OsStr,
    create_new: bool,
) -> io::Result<File> {
    let cstring = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;

    let mut flags = libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if create_new {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }

    // SAFETY: `parent` is a live directory file descriptor and the `cstring` is valid for
    // `openat`; mode is only consulted when creating a new file.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            cstring.as_ptr(),
            flags,
            0o666 as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `fd` was returned from `openat` and is uniquely owned by this branch.
    Ok(unsafe { File::from_raw_fd(fd) })
}

// ---------------------------------------------------------------------------
// FsListTool
// ---------------------------------------------------------------------------

/// Lists directory entries within the registered repository scope.
#[derive(Debug, Clone)]
pub struct FsListTool {
    repository_root: PathBuf,
}

impl FsListTool {
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
        }
    }
}

impl crate::runtime::RuntimeTool for FsListTool {
    fn tool_id(&self) -> &'static str {
        "fs.list"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.list"
    }

    fn declared_effect(&self) -> &'static str {
        "list directory entries within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        format!("path={}", request.resource_scope.value)
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        resolved_path_for_request(&self.repository_root, request)
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.list.v1";
        let relative = target_from_request(request);

        let canonical = match canonicalize_within_scope(&self.repository_root, &relative) {
            Ok(c) => c,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "list failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        let entries = match fs::read_dir(&canonical.canonical) {
            Ok(rd) => rd,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "list failed: cannot read directory".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
                );
            }
        };

        let mut names: Vec<String> = Vec::new();
        let mut file_count: u64 = 0;
        let mut dir_count: u64 = 0;
        let mut unknown_count: u64 = 0;

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => dir_count += 1,
                Ok(ft) if ft.is_file() => file_count += 1,
                Ok(_) => unknown_count += 1,
                Err(_) => match entry.metadata() {
                    Ok(meta) if meta.is_dir() => dir_count += 1,
                    Ok(meta) if meta.is_file() => file_count += 1,
                    _ => unknown_count += 1,
                },
            }
            names.push(name);
        }

        names.sort();

        let summary = format!(
            "{} entries in {} ({} files, {} dirs{})",
            names.len(),
            canonical.display_path(),
            file_count,
            dir_count,
            if unknown_count > 0 {
                format!(", {} other", unknown_count)
            } else {
                String::new()
            },
        );

        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(canonical.display_path()),
                },
                ToolResultField {
                    key: "entry_count".to_string(),
                    value: StructuredValue::Integer(names.len() as i64),
                },
                ToolResultField {
                    key: "entries".to_string(),
                    value: StructuredValue::StringList(names),
                },
                ToolResultField {
                    key: "file_count".to_string(),
                    value: StructuredValue::Integer(file_count as i64),
                },
                ToolResultField {
                    key: "dir_count".to_string(),
                    value: StructuredValue::Integer(dir_count as i64),
                },
                ToolResultField {
                    key: "unknown_count".to_string(),
                    value: StructuredValue::Integer(unknown_count as i64),
                },
            ],
            None,
            Vec::new(),
        )
    }
}

// ---------------------------------------------------------------------------
// FsStatTool
// ---------------------------------------------------------------------------

/// Reads file or directory metadata within the registered repository scope.
#[derive(Debug, Clone)]
pub struct FsStatTool {
    repository_root: PathBuf,
}

impl FsStatTool {
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
        }
    }
}

impl crate::runtime::RuntimeTool for FsStatTool {
    fn tool_id(&self) -> &'static str {
        "fs.stat"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.stat"
    }

    fn declared_effect(&self) -> &'static str {
        "read file or directory metadata within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        format!("path={}", request.resource_scope.value)
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        resolved_path_for_request(&self.repository_root, request)
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.stat.v1";
        let relative = target_from_request(request);

        let canonical = match canonicalize_within_scope(&self.repository_root, &relative) {
            Ok(c) => c,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "stat failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        let metadata = match fs::symlink_metadata(&canonical.canonical) {
            Ok(m) => m,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "stat failed: cannot read metadata".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
                );
            }
        };

        let file_type = if metadata.is_file() {
            "file"
        } else if metadata.is_dir() {
            "directory"
        } else {
            "other"
        };

        let size = metadata.len();
        let readonly = metadata.permissions().readonly();

        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let summary = format!(
            "{} {} ({} bytes{})",
            file_type,
            canonical.display_path(),
            size,
            if readonly { ", readonly" } else { "" }
        );

        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(canonical.display_path()),
                },
                ToolResultField {
                    key: "file_type".to_string(),
                    value: StructuredValue::String(file_type.to_string()),
                },
                ToolResultField {
                    key: "size_bytes".to_string(),
                    value: StructuredValue::Integer(size as i64),
                },
                ToolResultField {
                    key: "readonly".to_string(),
                    value: StructuredValue::Bool(readonly),
                },
                ToolResultField {
                    key: "modified_ms".to_string(),
                    value: StructuredValue::Integer(modified_ms),
                },
            ],
            None,
            Vec::new(),
        )
    }
}

// ---------------------------------------------------------------------------
// FsReadTool
// ---------------------------------------------------------------------------

/// Reads a bounded text window from a file within the registered repository scope.
#[derive(Debug, Clone)]
pub struct FsReadTool {
    repository_root: PathBuf,
    artifact_store: Option<LocalArtifactStore>,
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
    max_bytes: Option<usize>,
    max_scan_bytes: u64,
    max_file_bytes: Option<u64>,
    include_line_numbers: bool,
}

impl FsReadTool {
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
            artifact_store: None,
            start_line: 1,
            max_lines: DEFAULT_READ_MAX_LINES,
            max_chars: DEFAULT_READ_MAX_CHARS,
            max_bytes: None,
            max_scan_bytes: DEFAULT_READ_MAX_SCAN_BYTES,
            max_file_bytes: None,
            include_line_numbers: false,
        }
    }

    pub fn with_window(mut self, start_line: usize, max_lines: usize) -> Self {
        self.start_line = start_line.max(1);
        self.max_lines = max_lines;
        self
    }

    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars;
        self
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    pub fn with_artifact_store(mut self, artifact_store: LocalArtifactStore) -> Self {
        self.artifact_store = Some(artifact_store);
        self
    }

    pub fn with_max_scan_bytes(mut self, max_scan_bytes: u64) -> Self {
        self.max_scan_bytes = max_scan_bytes;
        self
    }

    pub fn with_max_file_bytes(mut self, max_file_bytes: u64) -> Self {
        self.max_file_bytes = Some(max_file_bytes);
        self
    }

    pub fn with_line_numbers(mut self) -> Self {
        self.include_line_numbers = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadTarget {
    Repository(CanonicalPath),
    Artifact(PathBuf),
}

impl ReadTarget {
    fn path(&self) -> &Path {
        match self {
            Self::Repository(canonical) => &canonical.canonical,
            Self::Artifact(path) => path,
        }
    }

    fn display_path(&self) -> String {
        match self {
            Self::Repository(canonical) => canonical.display_path(),
            Self::Artifact(path) => path.to_string_lossy().to_string(),
        }
    }
}

impl FsReadTool {
    fn resolve_target(&self, request: &RuntimeJobRequest) -> Result<ReadTarget, String> {
        self.resolve_target_for_request(request, None)
    }

    fn resolve_artifact_target_from_store(
        &self,
        request: &RuntimeJobRequest,
    ) -> Result<PathBuf, String> {
        let output_ref_id = OutputRefId::try_from_string(&request.resource_scope.value)
            .map_err(|error| error.to_string())?;
        let store = self
            .artifact_store
            .as_ref()
            .ok_or_else(|| "artifact store is not configured".to_string())?;
        let repository_scope = Some(request.repository_id.as_str());

        match store
            .resolve_output_record_for_context(
                &output_ref_id,
                Some(request.repository_id.as_str()),
                repository_scope,
                None,
            )
            .map_err(|error| error.to_string())?
        {
            ArtifactLookupResult::Found(record) => Ok(record.path),
            ArtifactLookupResult::NotFound => Err("artifact not found".to_string()),
            ArtifactLookupResult::Denied(ArtifactLookupDenyReason::ProjectOrJobContextRequired) => {
                Err(
                    "artifact requires project/job context, but this request does not include one"
                        .to_string(),
                )
            }
            ArtifactLookupResult::Denied(
                ArtifactLookupDenyReason::RepositoryScopeOrIdentityMismatch,
            ) => Err("artifact is not accessible from this repository context".to_string()),
        }
    }

    fn resolve_target_for_request(
        &self,
        request: &RuntimeJobRequest,
        invocation: Option<&ToolInvocation>,
    ) -> Result<ReadTarget, String> {
        if matches!(
            request.resource_scope.kind.as_str(),
            "artifact" | "artifact_ref"
        ) {
            if let Some(invocation) = invocation {
                if let Some(resolved_path) = invocation
                    .resolved_paths
                    .iter()
                    .find(|resolved_path| {
                        resolved_path.requested_path == request.resource_scope.value
                            && resolved_path.resolved_path != request.resource_scope.value
                    })
                    .map(|resolved_path| PathBuf::from(&resolved_path.resolved_path))
                {
                    if let Ok(store_path) = self.resolve_artifact_target_from_store(request) {
                        if store_path == resolved_path {
                            return Ok(ReadTarget::Artifact(resolved_path));
                        }
                    }
                }
            }

            return Ok(ReadTarget::Artifact(
                self.resolve_artifact_target_from_store(request)?,
            ));
        }

        let relative = target_from_request(request);
        canonicalize_within_scope(&self.repository_root, &relative)
            .map(ReadTarget::Repository)
            .map_err(|error| error.to_string())
    }

    fn resolve_target_for_request_path(
        &self,
        request: &RuntimeJobRequest,
        invocation: Option<&ToolInvocation>,
    ) -> Vec<ResolvedPath> {
        if matches!(
            request.resource_scope.kind.as_str(),
            "artifact" | "artifact_ref"
        ) {
            return match self.resolve_artifact_target_from_store(request) {
                Ok(path) => vec![ResolvedPath {
                    requested_path: request.resource_scope.value.clone(),
                    resolved_path: path.to_string_lossy().to_string(),
                    display_path: path.display().to_string(),
                }],
                Err(_) => Vec::new(),
            };
        }

        match self.resolve_target_for_request(request, invocation) {
            Ok(target) => vec![ResolvedPath {
                requested_path: request.resource_scope.value.clone(),
                resolved_path: target.path().to_string_lossy().to_string(),
                display_path: target.display_path(),
            }],
            Err(_) => resolved_path_for_request(&self.repository_root, request),
        }
    }

    fn open_read_target(&self, target: &ReadTarget) -> io::Result<File> {
        match target {
            ReadTarget::Repository(canonical) => open_canonical_file_within_scope(canonical),
            ReadTarget::Artifact(path) => open_artifact_file_within_scope(path),
        }
    }

    fn display_target_path(&self, target: &ReadTarget) -> String {
        target.display_path()
    }
}

impl crate::runtime::RuntimeTool for FsReadTool {
    fn tool_id(&self) -> &'static str {
        "fs.read"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.read"
    }

    fn declared_effect(&self) -> &'static str {
        "read a bounded text window within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        let mut parts = vec![format!("path={}", request.resource_scope.value)];
        if self.start_line != 1 || self.max_lines != DEFAULT_READ_MAX_LINES {
            parts.push(format!("window={}:{}", self.start_line, self.max_lines));
        }
        if self.max_chars != DEFAULT_READ_MAX_CHARS {
            parts.push(format!("chars={}", self.max_chars));
        }
        if let Some(max_bytes) = self.max_bytes {
            parts.push(format!("bytes={}", max_bytes));
        }
        if self.max_scan_bytes != DEFAULT_READ_MAX_SCAN_BYTES {
            parts.push(format!("scan={}", self.max_scan_bytes));
        }
        if let Some(max_file_bytes) = self.max_file_bytes {
            parts.push(format!("file={max_file_bytes}"));
        }
        if self.include_line_numbers {
            parts.push("ln=true".to_string());
        }
        parts.join(" ")
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        if matches!(
            request.resource_scope.kind.as_str(),
            "artifact" | "artifact_ref"
        ) {
            return self.resolve_target_for_request_path(request, None);
        }

        match self.resolve_target(request) {
            Ok(target) => vec![ResolvedPath {
                requested_path: request.resource_scope.value.clone(),
                resolved_path: target.path().to_string_lossy().to_string(),
                display_path: target.display_path(),
            }],
            Err(_) => resolved_path_for_request(&self.repository_root, request),
        }
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.read.v1";
        let target = match self.resolve_target_for_request(request, Some(invocation)) {
            Ok(target) => target,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: invalid read target".to_string(),
                    err,
                );
            }
        };

        let file = match self.open_read_target(&target) {
            Ok(file) => file,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: cannot open scoped file".to_string(),
                    format!("{}: {}", self.display_target_path(&target), err),
                );
            }
        };

        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: cannot read file metadata".to_string(),
                    format!("{}: {}", self.display_target_path(&target), err),
                );
            }
        };

        if !metadata.is_file() {
            return failed_result(
                invocation,
                schema_ref,
                "read failed: target is not a file".to_string(),
                self.display_target_path(&target),
            );
        }

        if let Some(max_file_bytes) = self.max_file_bytes {
            if metadata.len() > max_file_bytes {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: file exceeds configured byte limit".to_string(),
                    format!(
                        "{} is {} bytes; limit is {} bytes",
                        self.display_target_path(&target),
                        metadata.len(),
                        max_file_bytes
                    ),
                );
            }
        }

        let window = match read_text_window(
            file,
            self.start_line,
            self.max_lines,
            self.max_chars,
            self.max_bytes,
            self.max_scan_bytes,
            self.include_line_numbers,
        ) {
            Ok(window) => window,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: cannot read UTF-8 text".to_string(),
                    format!("{}: {}", self.display_target_path(&target), err),
                );
            }
        };

        let mut truncation_reasons = Vec::new();
        if self.start_line > 1 {
            truncation_reasons.push(format!("window starts at line {}", self.start_line));
        }
        if window.truncated_by_lines {
            truncation_reasons.push(format!("line limit {}", self.max_lines));
        }
        if window.truncated_by_chars {
            truncation_reasons.push(format!("character limit {}", self.max_chars));
        }
        if window.truncated_by_bytes {
            truncation_reasons.push(format!("byte limit {}", self.max_bytes.unwrap_or_default()));
        }
        if window.truncated_by_scan {
            truncation_reasons.push(format!("scan byte limit {}", self.max_scan_bytes));
        }
        let truncation = if truncation_reasons.is_empty() {
            None
        } else {
            Some(TruncationMetadata {
                original_bytes: metadata.len(),
                retained_bytes: window.retained_source_bytes,
                reason: truncation_reasons.join("; "),
            })
        };

        let summary = format!(
            "read {} line(s) from {}",
            window.line_count,
            self.display_target_path(&target)
        );
        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(self.display_target_path(&target)),
                },
                ToolResultField {
                    key: "content".to_string(),
                    value: StructuredValue::String(window.content),
                },
                ToolResultField {
                    key: "start_line".to_string(),
                    value: StructuredValue::Integer(self.start_line as i64),
                },
                ToolResultField {
                    key: "end_line".to_string(),
                    value: StructuredValue::Integer(window.end_line as i64),
                },
                ToolResultField {
                    key: "line_count".to_string(),
                    value: StructuredValue::Integer(window.line_count as i64),
                },
                ToolResultField {
                    key: "file_size_bytes".to_string(),
                    value: StructuredValue::Integer(metadata.len() as i64),
                },
            ],
            truncation,
            Vec::new(),
        )
    }
}
// ---------------------------------------------------------------------------
// FsSearchTool
// ---------------------------------------------------------------------------

/// Searches file contents for a literal pattern within the registered repository scope.
#[derive(Debug, Clone)]
pub struct FsSearchTool {
    repository_root: PathBuf,
    pattern: String,
    max_results: usize,
    max_file_bytes: u64,
}

impl FsSearchTool {
    pub fn new(repository_root: impl Into<PathBuf>, pattern: impl Into<String>) -> Self {
        Self {
            repository_root: repository_root.into(),
            pattern: pattern.into(),
            max_results: DEFAULT_SEARCH_MAX_RESULTS,
            max_file_bytes: DEFAULT_SEARCH_MAX_FILE_BYTES,
        }
    }

    pub fn with_max_results(mut self, max: usize) -> Self {
        self.max_results = max;
        self
    }

    pub fn with_max_file_bytes(mut self, max: u64) -> Self {
        self.max_file_bytes = max;
        self
    }
}

impl crate::runtime::RuntimeTool for FsSearchTool {
    fn tool_id(&self) -> &'static str {
        "fs.search"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.search"
    }

    fn declared_effect(&self) -> &'static str {
        "search file contents for a pattern within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        format!(
            "path={} pattern={}",
            request.resource_scope.value, self.pattern
        )
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        resolved_path_for_request(&self.repository_root, request)
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.search.v1";
        let relative = target_from_request(request);

        let canonical = match canonicalize_within_scope(&self.repository_root, &relative) {
            Ok(c) => c,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "search failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        let mut matches: Vec<String> = Vec::new();
        let mut files_searched: u64 = 0;
        let mut files_skipped: u64 = 0;
        let mut truncated = false;
        let mut total_bytes: u64 = 0;
        let mut retained_bytes: u64 = 0;

        if canonical.canonical.is_file() {
            let metadata = match fs::metadata(&canonical.canonical) {
                Ok(metadata) => metadata,
                Err(err) => {
                    return failed_result(
                        invocation,
                        schema_ref,
                        "search failed: cannot read file metadata".to_string(),
                        format!("{}: {}", canonical.display_path(), err),
                    );
                }
            };
            if metadata.len() > self.max_file_bytes {
                files_skipped = 1;
            } else if let Err(err) = search_file(
                &canonical.canonical,
                &canonical.root,
                &self.pattern,
                self.max_results,
                &mut matches,
                &mut truncated,
                &mut total_bytes,
                &mut retained_bytes,
            ) {
                return failed_result(
                    invocation,
                    schema_ref,
                    "search failed: cannot read file".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
                );
            } else {
                files_searched = 1;
            }
        } else if canonical.canonical.is_dir() {
            let mut visited_dirs = HashSet::from([canonical.canonical.clone()]);
            if let Err(err) = search_recursive(
                &canonical.canonical,
                &canonical.root,
                &self.pattern,
                self.max_results,
                self.max_file_bytes,
                &mut matches,
                &mut files_searched,
                &mut files_skipped,
                &mut truncated,
                &mut total_bytes,
                &mut retained_bytes,
                &mut visited_dirs,
            ) {
                return failed_result(
                    invocation,
                    schema_ref,
                    "search failed: cannot traverse directory".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
                );
            }
        }

        let truncation = if truncated {
            Some(TruncationMetadata {
                original_bytes: total_bytes,
                retained_bytes,
                reason: format!("search results truncated at {} matches", self.max_results),
            })
        } else {
            None
        };

        let match_count = matches.len();
        let summary = format!(
            "{} match(es) in {} file(s) under {}",
            match_count,
            files_searched,
            canonical.display_path(),
        );

        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(canonical.display_path()),
                },
                ToolResultField {
                    key: "pattern".to_string(),
                    value: StructuredValue::String(self.pattern.clone()),
                },
                ToolResultField {
                    key: "matches".to_string(),
                    value: StructuredValue::StringList(matches),
                },
                ToolResultField {
                    key: "match_count".to_string(),
                    value: StructuredValue::Integer(match_count as i64),
                },
                ToolResultField {
                    key: "files_searched".to_string(),
                    value: StructuredValue::Integer(files_searched as i64),
                },
                ToolResultField {
                    key: "files_skipped".to_string(),
                    value: StructuredValue::Integer(files_skipped as i64),
                },
            ],
            truncation,
            Vec::new(),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn search_recursive(
    dir: &Path,
    root: &Path,
    pattern: &str,
    max_results: usize,
    max_file_bytes: u64,
    matches: &mut Vec<String>,
    files_searched: &mut u64,
    files_skipped: &mut u64,
    truncated: &mut bool,
    total_bytes: &mut u64,
    retained_bytes: &mut u64,
    visited_dirs: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !canonical.starts_with(root) {
            continue;
        }

        let metadata = fs::metadata(&canonical)?;
        if metadata.is_dir() {
            if !visited_dirs.insert(canonical.clone()) {
                continue;
            }
            search_recursive(
                &canonical,
                root,
                pattern,
                max_results,
                max_file_bytes,
                matches,
                files_searched,
                files_skipped,
                truncated,
                total_bytes,
                retained_bytes,
                visited_dirs,
            )?;
        } else if metadata.is_file() {
            if metadata.len() > max_file_bytes {
                *files_skipped += 1;
                continue;
            }
            *files_searched += 1;
            let snapshot_len = matches.len();
            let snapshot_total = *total_bytes;
            let snapshot_retained = *retained_bytes;
            let snapshot_truncated = *truncated;
            if search_file(
                &canonical,
                root,
                pattern,
                max_results,
                matches,
                truncated,
                total_bytes,
                retained_bytes,
            )
            .is_err()
            {
                matches.truncate(snapshot_len);
                *total_bytes = snapshot_total;
                *retained_bytes = snapshot_retained;
                *truncated = snapshot_truncated;
                *files_searched -= 1;
                *files_skipped += 1;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadWindow {
    content: String,
    end_line: usize,
    line_count: usize,
    retained_source_bytes: u64,
    truncated_by_lines: bool,
    truncated_by_chars: bool,
    truncated_by_bytes: bool,
    truncated_by_scan: bool,
}

fn read_text_window(
    file: File,
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
    max_bytes: Option<usize>,
    max_scan_bytes: u64,
    include_line_numbers: bool,
) -> io::Result<ReadWindow> {
    let mut reader = io::BufReader::new(file);
    let start_line = start_line.max(1);

    let mut content = String::new();
    let mut end_line = 0;
    let mut line_count = 0;
    let mut used_chars = 0;
    let mut used_bytes = 0;
    let mut retained_source_bytes = 0;
    let mut scanned_bytes = 0;
    let mut previous_retained_newline_bytes = 0;
    let mut truncated_by_lines = false;
    let mut truncated_by_chars = false;
    let mut truncated_by_bytes = false;
    let mut truncated_by_scan = false;
    let mut current_line = 0;
    let mut line_bytes = Vec::new();

    loop {
        if current_line >= start_line {
            if line_count >= max_lines {
                truncated_by_lines = !reader.fill_buf()?.is_empty();
                break;
            }
            if used_chars >= max_chars {
                truncated_by_chars = !reader.fill_buf()?.is_empty();
                break;
            }
            if let Some(max_bytes_limit) = max_bytes {
                if used_bytes >= max_bytes_limit {
                    truncated_by_bytes = !reader.fill_buf()?.is_empty();
                    break;
                }
            }
        }

        if scanned_bytes >= max_scan_bytes {
            truncated_by_scan = !reader.fill_buf()?.is_empty();
            break;
        }

        line_bytes.clear();
        let remaining_scan = max_scan_bytes - scanned_bytes;
        let bytes_read = reader
            .by_ref()
            .take(remaining_scan)
            .read_until(b'\n', &mut line_bytes)?;
        if bytes_read == 0 {
            break;
        }

        scanned_bytes += bytes_read as u64;
        current_line += 1;
        let ended_with_newline = line_bytes.last() == Some(&b'\n');
        let mut source_newline_bytes = 0;
        if ended_with_newline {
            line_bytes.pop();
            source_newline_bytes = 1;
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
                source_newline_bytes += 1;
            }
        } else if scanned_bytes >= max_scan_bytes {
            truncated_by_scan = !reader.fill_buf()?.is_empty();
        }

        if current_line < start_line {
            if truncated_by_scan {
                break;
            }
            continue;
        }

        if line_bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "binary file rejected (contains NUL byte)",
            ));
        }

        let line = match std::str::from_utf8(&line_bytes) {
            Ok(line) => line.to_string(),
            Err(error) if truncated_by_scan && error.error_len().is_none() => {
                let valid_up_to = error.valid_up_to();
                line_bytes.truncate(valid_up_to);
                std::str::from_utf8(&line_bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                    .to_string()
            }
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        let rendered = if include_line_numbers {
            format!("{current_line}: {line}")
        } else {
            line.clone()
        };
        let separator_chars = usize::from(!content.is_empty());
        if used_chars + separator_chars >= max_chars {
            truncated_by_chars = true;
            break;
        }
        let separator_bytes = separator_chars;
        if let Some(max_bytes_limit) = max_bytes {
            if used_bytes + separator_bytes > max_bytes_limit {
                truncated_by_bytes = true;
                break;
            }
        }

        if separator_chars == 1 {
            content.push('\n');
            used_chars += 1;
            used_bytes += 1;
            retained_source_bytes += previous_retained_newline_bytes;
        }

        let remaining_chars = max_chars - used_chars;
        let rendered_chars = rendered.chars().count();
        if rendered_chars > remaining_chars {
            content.extend(rendered.chars().take(remaining_chars));
            retained_source_bytes += retained_source_bytes_for_rendered_chars(
                &line,
                current_line,
                include_line_numbers,
                remaining_chars,
            );
            line_count += 1;
            end_line = current_line;
            truncated_by_chars = true;
            break;
        }

        let rendered_bytes = rendered.len();
        if let Some(max_bytes_limit) = max_bytes {
            if used_bytes + rendered_bytes > max_bytes_limit {
                let available_bytes = max_bytes_limit.saturating_sub(used_bytes);
                let rendered = truncate_utf8_to_byte_boundary(&rendered, available_bytes);
                let rendered_bytes = rendered.len();
                if rendered_bytes > 0 {
                    content.push_str(rendered);
                    retained_source_bytes += retained_source_bytes_for_rendered_bytes(
                        &line,
                        current_line,
                        include_line_numbers,
                        rendered_bytes,
                    );
                    line_count += 1;
                    end_line = current_line;
                }
                truncated_by_bytes = true;
                break;
            }
        }

        content.push_str(&rendered);
        used_chars += rendered_chars;
        used_bytes += rendered_bytes;
        retained_source_bytes += line.len() as u64;
        line_count += 1;
        end_line = current_line;
        previous_retained_newline_bytes = source_newline_bytes;

        if truncated_by_scan {
            break;
        }
    }

    Ok(ReadWindow {
        content,
        end_line,
        line_count,
        retained_source_bytes,
        truncated_by_lines,
        truncated_by_chars,
        truncated_by_bytes,
        truncated_by_scan,
    })
}

fn retained_source_bytes_for_rendered_chars(
    line: &str,
    current_line: usize,
    include_line_numbers: bool,
    rendered_char_count: usize,
) -> u64 {
    let source_char_count = if include_line_numbers {
        let prefix_chars = format!("{current_line}: ").chars().count();
        rendered_char_count.saturating_sub(prefix_chars)
    } else {
        rendered_char_count
    };

    line.chars()
        .take(source_char_count)
        .map(|character| character.len_utf8() as u64)
        .sum()
}

fn truncate_utf8_to_byte_boundary(input: &str, byte_limit: usize) -> &str {
    if byte_limit >= input.len() {
        return input;
    }

    let mut cut = byte_limit;
    while !input.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    &input[..cut]
}

fn retained_source_bytes_for_rendered_bytes(
    line: &str,
    current_line: usize,
    include_line_numbers: bool,
    rendered_byte_count: usize,
) -> u64 {
    if rendered_byte_count == 0 {
        return 0;
    }

    let prefix_bytes = if include_line_numbers {
        format!("{current_line}: ").len()
    } else {
        0
    };

    let source_bytes = rendered_byte_count
        .saturating_sub(prefix_bytes)
        .min(line.len());
    u64::try_from(source_bytes).unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn search_file(
    path: &Path,
    root: &Path,
    pattern: &str,
    max_results: usize,
    matches: &mut Vec<String>,
    truncated: &mut bool,
    total_bytes: &mut u64,
    retained_bytes: &mut u64,
) -> io::Result<()> {
    let file = fs::File::open(path)?;

    let reader = io::BufReader::new(file);
    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.contains(pattern) {
            let match_ref = format!("{}:{}", relative, line_num + 1);
            *total_bytes += match_ref.len() as u64;
            if matches.len() < max_results {
                *retained_bytes += match_ref.len() as u64;
                matches.push(match_ref);
            } else {
                *truncated = true;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// FsWriteTool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct MutationTarget {
    canonical: PathBuf,
    display_path: String,
}

impl MutationTarget {
    fn path(&self) -> &Path {
        &self.canonical
    }

    fn display_path(&self) -> &str {
        &self.display_path
    }
}

fn resolve_mutation_target(
    repository_root: &Path,
    relative_path: &Path,
) -> Result<MutationTarget, PathResolutionError> {
    let canonical_root = repository_root
        .canonicalize()
        .map_err(|_| PathResolutionError::RootNotFound)?;

    let target = if relative_path.is_absolute() {
        relative_path.to_path_buf()
    } else {
        canonical_root.join(relative_path)
    };

    if target.exists() {
        let canonical_target =
            target
                .canonicalize()
                .map_err(|_| PathResolutionError::TargetNotFound {
                    requested: target.clone(),
                })?;

        if !canonical_target.starts_with(&canonical_root) {
            return Err(PathResolutionError::OutsideRepositoryScope {
                resolved: canonical_target,
                root: canonical_root,
            });
        }

        let display_path = canonical_target
            .strip_prefix(&canonical_root)
            .unwrap_or(&canonical_target)
            .to_string_lossy()
            .to_string();

        return Ok(MutationTarget {
            canonical: canonical_target,
            display_path,
        });
    }

    let file_name = target
        .file_name()
        .ok_or(PathResolutionError::TargetNotFound {
            requested: target.clone(),
        })?;
    if file_name == "." || file_name == ".." {
        return Err(PathResolutionError::TargetNotFound {
            requested: target.clone(),
        });
    }

    let parent = target.parent().ok_or(PathResolutionError::TargetNotFound {
        requested: target.clone(),
    })?;
    let canonical_parent = canonicalize_within_scope(repository_root, parent)?;
    let canonical_target = canonical_parent.canonical.join(file_name);

    if !canonical_target.starts_with(&canonical_root) {
        return Err(PathResolutionError::OutsideRepositoryScope {
            resolved: canonical_target,
            root: canonical_root,
        });
    }

    let display_path = canonical_target
        .strip_prefix(&canonical_root)
        .unwrap_or(&canonical_target)
        .to_string_lossy()
        .to_string();

    Ok(MutationTarget {
        canonical: canonical_target,
        display_path,
    })
}

fn open_write_file_no_follow(path: &Path, create_new: bool) -> io::Result<File> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path does not name a file in its parent",
        )
    })?;

    #[cfg(unix)]
    let expected_metadata = if create_new {
        None
    } else {
        let metadata = fs::metadata(path)?;
        Some((metadata.dev(), metadata.ino()))
    };

    #[cfg(target_os = "linux")]
    let expected_path = if create_new {
        None
    } else {
        Some(path.canonicalize()?)
    };

    #[cfg(unix)]
    {
        let parent_dir = open_parent_no_follow(parent)?;
        let file = open_no_follow_in_parent_dir(&parent_dir, file_name, create_new)?;

        #[cfg(unix)]
        {
            if let Some((expected_dev, expected_ino)) = expected_metadata {
                let opened_metadata = file.metadata()?;
                if opened_metadata.dev() != expected_dev || opened_metadata.ino() != expected_ino {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "opened write file did not match resolved record",
                    ));
                }
            }

            #[cfg(target_os = "linux")]
            if let Some(expected_path) = expected_path {
                let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
                let opened_path = fs::canonicalize(fd_path)?;
                if opened_path != expected_path {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "opened write file path changed after resolution",
                    ));
                }
            }
        }

        Ok(file)
    }

    #[cfg(not(unix))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem write/patch is best-effort on non-Unix platforms",
    ))
}

#[cfg(unix)]
fn temporary_write_file_name(file_name: &std::ffi::OsStr, attempt: u32) -> std::ffi::OsString {
    let file_name = file_name.to_string_lossy();
    let attempt = WRITE_FILE_TMP_COUNTER.fetch_add(1, Ordering::SeqCst) + attempt as u64;
    format!("{WRITE_FILE_TMP_PREFIX}-{file_name}-{attempt}").into()
}

#[cfg(unix)]
fn create_temporary_file_in_parent_dir(
    parent: &File,
    file_name: &std::ffi::OsStr,
) -> io::Result<(std::ffi::OsString, File)> {
    for attempt in 0..128u32 {
        let temp_name = temporary_write_file_name(file_name, attempt);
        match open_no_follow_in_parent_dir(parent, &temp_name, true) {
            Ok(file) => return Ok((temp_name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create temporary write file",
    ))
}

#[cfg(unix)]
fn rename_in_parent_dir(
    parent: &File,
    source: &std::ffi::OsStr,
    destination: &std::ffi::OsStr,
) -> io::Result<()> {
    let source = CString::new(source.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    let destination = CString::new(destination.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;

    // SAFETY: `parent` is a live directory file descriptor and both names are valid c-strings.
    let result = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
        )
    };

    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(unix)]
fn unlink_in_parent_dir(parent: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    let cstring = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;

    // SAFETY: `parent` is a live directory file descriptor and `cstring` is a valid name.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), cstring.as_ptr(), 0) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error);
    }

    Ok(())
}

#[cfg(unix)]
fn write_file_bytes_atomically_inner(
    path: &Path,
    bytes: &[u8],
    create_new: bool,
    fail_after_write: bool,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;

    let parent_dir = open_parent_no_follow(parent)?;
    let existing_permissions = if !create_new {
        let existing_file = open_write_file_no_follow(path, false)?;
        let existing_metadata = existing_file.metadata()?;
        Some(existing_metadata.permissions())
    } else {
        None
    };

    let (temp_name, mut temp_file) = create_temporary_file_in_parent_dir(&parent_dir, file_name)?;

    let cleanup = |parent_dir: &File,
                   temp_name: &std::ffi::OsStr,
                   file_name: &std::ffi::OsStr,
                   create_new: bool| {
        let _ = unlink_in_parent_dir(parent_dir, temp_name);
        if create_new {
            let _ = unlink_in_parent_dir(parent_dir, file_name);
        }
    };

    let write_result = (|| -> io::Result<()> {
        temp_file.write_all(bytes)?;
        temp_file.flush()?;
        temp_file.sync_all()?;
        if let Some(permissions) = existing_permissions {
            temp_file.set_permissions(permissions)?;
        }
        if fail_after_write {
            return Err(io::Error::other("simulated write failure"));
        }
        drop(temp_file);

        let _destination_guard = if create_new {
            open_write_file_no_follow(path, true)?
        } else {
            open_write_file_no_follow(path, false)?
        };
        rename_in_parent_dir(&parent_dir, &temp_name, file_name)?;
        Ok(())
    })();

    if let Err(error) = write_result {
        cleanup(&parent_dir, &temp_name, file_name, create_new);
        return Err(error);
    }

    parent_dir.sync_all()?;

    Ok(())
}

#[cfg(unix)]
fn write_file_bytes_atomically(path: &Path, bytes: &[u8], create_new: bool) -> io::Result<()> {
    write_file_bytes_atomically_inner(path, bytes, create_new, false)
}

#[cfg(not(unix))]
fn write_file_bytes_atomically(_path: &Path, _bytes: &[u8], _create_new: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem write/patch is best-effort on non-Unix platforms",
    ))
}

fn read_entire_text_file(path: &Path, max_bytes: usize) -> io::Result<String> {
    let mut file = open_file_no_follow(path)?;
    let mut content = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut total_bytes = 0usize;

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }

        total_bytes += n;
        if total_bytes > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "file exceeds configured byte limit",
            ));
        }

        content.extend_from_slice(&buffer[..n]);
    }

    String::from_utf8(content).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 content: {error}"),
        )
    })
}

fn count_overlapping_matches(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }

    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return 0;
    }

    let mut count = 0usize;
    let mut index = 0usize;
    while index + needle.len() <= haystack.len() {
        if &haystack[index..index + needle.len()] == needle {
            count += 1;
        }
        index += 1;
    }
    count
}

#[derive(Debug, Clone)]
pub struct FsWriteTool {
    repository_root: PathBuf,
    content: String,
    allow_create: bool,
    allow_overwrite: bool,
    max_bytes: usize,
}

impl FsWriteTool {
    pub fn new(repository_root: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self {
            repository_root: repository_root.into(),
            content: content.into(),
            allow_create: true,
            allow_overwrite: false,
            max_bytes: DEFAULT_WRITE_MAX_BYTES,
        }
    }

    pub fn with_allow_create(mut self, allow_create: bool) -> Self {
        self.allow_create = allow_create;
        self
    }

    pub fn with_allow_overwrite(mut self, allow_overwrite: bool) -> Self {
        self.allow_overwrite = allow_overwrite;
        self
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl crate::runtime::RuntimeTool for FsWriteTool {
    fn tool_id(&self) -> &'static str {
        "fs.write"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.write"
    }

    fn declared_effect(&self) -> &'static str {
        "write UTF-8 text within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        let mut parts = vec![format!("path={}", request.resource_scope.value)];
        parts.push(format!("bytes={}", self.content.len()));
        if self.allow_create {
            parts.push("create=true".to_string());
        }
        if self.allow_overwrite {
            parts.push("overwrite=true".to_string());
        }
        if self.max_bytes != DEFAULT_WRITE_MAX_BYTES {
            parts.push(format!("limit={}", self.max_bytes));
        }
        parts.join(" ")
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        match resolve_mutation_target(&self.repository_root, &target_from_request(request)) {
            Ok(target) => vec![ResolvedPath {
                requested_path: request.resource_scope.value.clone(),
                resolved_path: target.path().to_string_lossy().to_string(),
                display_path: target.display_path().to_string(),
            }],
            Err(_) => Vec::new(),
        }
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.write.v1";
        let relative = target_from_request(request);

        if self.content.len() > self.max_bytes {
            return failed_result(
                invocation,
                schema_ref,
                "write failed: content exceeds configured byte limit".to_string(),
                format!(
                    "content is {} bytes; limit is {} bytes",
                    self.content.len(),
                    self.max_bytes
                ),
            );
        }

        let target = match resolve_mutation_target(&self.repository_root, &relative) {
            Ok(target) => target,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "write failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        let path = target.path();
        let exists = path.exists();
        if exists && !self.allow_overwrite {
            return failed_result(
                invocation,
                schema_ref,
                "write failed: overwrite disabled".to_string(),
                format!("{}: overwrite disabled", target.display_path()),
            );
        }
        if !exists && !self.allow_create {
            return failed_result(
                invocation,
                schema_ref,
                "write failed: create disabled".to_string(),
                format!("{}: create disabled", target.display_path()),
            );
        }

        if exists {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    return failed_result(
                        invocation,
                        schema_ref,
                        "write failed: cannot read target metadata".to_string(),
                        format!("{}: {}", target.display_path(), err),
                    );
                }
            };
            if !metadata.is_file() {
                return failed_result(
                    invocation,
                    schema_ref,
                    "write failed: target is not a file".to_string(),
                    target.display_path().to_string(),
                );
            }
        }

        if let Err(err) = write_file_bytes_atomically(path, self.content.as_bytes(), !exists) {
            return failed_result(
                invocation,
                schema_ref,
                "write failed: cannot write UTF-8 text".to_string(),
                format!("{}: {}", target.display_path(), err),
            );
        }

        let summary = if exists {
            format!(
                "overwrote {} bytes in {}",
                self.content.len(),
                target.display_path()
            )
        } else {
            format!(
                "created {} bytes at {}",
                self.content.len(),
                target.display_path()
            )
        };

        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(target.display_path().to_string()),
                },
                ToolResultField {
                    key: "bytes_written".to_string(),
                    value: StructuredValue::Integer(self.content.len() as i64),
                },
                ToolResultField {
                    key: "created".to_string(),
                    value: StructuredValue::Bool(!exists),
                },
                ToolResultField {
                    key: "overwritten".to_string(),
                    value: StructuredValue::Bool(exists),
                },
            ],
            None,
            Vec::new(),
        )
    }
}

// ---------------------------------------------------------------------------
// FsPatchTool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FsPatchTool {
    repository_root: PathBuf,
    find_text: String,
    replacement_text: String,
    max_bytes: usize,
}

impl FsPatchTool {
    pub fn new(
        repository_root: impl Into<PathBuf>,
        find_text: impl Into<String>,
        replacement_text: impl Into<String>,
    ) -> Self {
        Self {
            repository_root: repository_root.into(),
            find_text: find_text.into(),
            replacement_text: replacement_text.into(),
            max_bytes: DEFAULT_WRITE_MAX_BYTES,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl crate::runtime::RuntimeTool for FsPatchTool {
    fn tool_id(&self) -> &'static str {
        "fs.patch"
    }

    fn requested_capability(&self) -> &'static str {
        "filesystem.patch"
    }

    fn declared_effect(&self) -> &'static str {
        "apply an exact-match text replacement within the registered repository scope"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        let mut parts = vec![format!("path={}", request.resource_scope.value)];
        parts.push(format!("find={}", self.find_text.len()));
        parts.push(format!("replace={}", self.replacement_text.len()));
        if self.max_bytes != DEFAULT_WRITE_MAX_BYTES {
            parts.push(format!("limit={}", self.max_bytes));
        }
        parts.join(" ")
    }

    fn resolved_paths(&self, request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        match resolve_mutation_target(&self.repository_root, &target_from_request(request)) {
            Ok(target) => vec![ResolvedPath {
                requested_path: request.resource_scope.value.clone(),
                resolved_path: target.path().to_string_lossy().to_string(),
                display_path: target.display_path().to_string(),
            }],
            Err(_) => Vec::new(),
        }
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.patch.v1";
        let relative = target_from_request(request);

        if self.find_text.is_empty() {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: empty match text".to_string(),
                "exact-match replacement requires a non-empty find string".to_string(),
            );
        }

        let target = match resolve_mutation_target(&self.repository_root, &relative) {
            Ok(target) => target,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "patch failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        let path = target.path();
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "patch failed: cannot read target metadata".to_string(),
                    format!("{}: {}", target.display_path(), err),
                );
            }
        };

        if !metadata.is_file() {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: target is not a file".to_string(),
                target.display_path().to_string(),
            );
        }

        let content = match read_entire_text_file(path, self.max_bytes) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::FileTooLarge => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "patch failed: file exceeds configured byte limit".to_string(),
                    format!(
                        "{} is larger than {} bytes",
                        target.display_path(),
                        self.max_bytes
                    ),
                );
            }
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "patch failed: cannot read UTF-8 text".to_string(),
                    format!("{}: {}", target.display_path(), err),
                );
            }
        };

        let match_count = count_overlapping_matches(&content, &self.find_text);
        if match_count == 0 {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: exact text not found".to_string(),
                format!(
                    "needle {:?} was not found in {}",
                    self.find_text,
                    target.display_path()
                ),
            );
        }
        if match_count > 1 {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: exact text matched multiple times".to_string(),
                format!(
                    "needle {:?} matched {} times in {}",
                    self.find_text,
                    match_count,
                    target.display_path()
                ),
            );
        }

        let updated = content.replacen(&self.find_text, &self.replacement_text, 1);
        if updated.len() > self.max_bytes {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: result exceeds configured byte limit".to_string(),
                format!(
                    "patched content is {} bytes; limit is {} bytes",
                    updated.len(),
                    self.max_bytes
                ),
            );
        }

        if let Err(err) = write_file_bytes_atomically(path, updated.as_bytes(), false) {
            return failed_result(
                invocation,
                schema_ref,
                "patch failed: cannot write UTF-8 text".to_string(),
                format!("{}: {}", target.display_path(), err),
            );
        }

        let summary = if updated == content {
            format!(
                "patched {} with no net content change",
                target.display_path()
            )
        } else {
            format!(
                "patched {} with one exact replacement",
                target.display_path()
            )
        };

        make_tool_result(
            invocation,
            ToolResultStatus::Succeeded,
            schema_ref,
            vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(summary),
                },
                ToolResultField {
                    key: "path".to_string(),
                    value: StructuredValue::String(target.display_path().to_string()),
                },
                ToolResultField {
                    key: "matches".to_string(),
                    value: StructuredValue::Integer(match_count as i64),
                },
                ToolResultField {
                    key: "before_bytes".to_string(),
                    value: StructuredValue::Integer(content.len() as i64),
                },
                ToolResultField {
                    key: "after_bytes".to_string(),
                    value: StructuredValue::Integer(updated.len() as i64),
                },
                ToolResultField {
                    key: "changed".to_string(),
                    value: StructuredValue::Bool(updated != content),
                },
            ],
            None,
            Vec::new(),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactStoreConfig, ArtifactWriteMetadata, LocalArtifactStore};
    use crate::domain::{
        Actor, JobKind, OutputRefId, RepositoryId, RepositoryRecord, RepositoryTrustState,
        ToolInvocationId,
    };
    use crate::runtime::{RuntimeTool, SecretaryRuntime, RUNTIME_SCHEMA_VERSION};
    use crate::store::SecretaryStore;
    use crate::tool_output::{render_tool_result, OutputFormat, RenderOptions};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("atelia-tools-test-{}-{}", id, name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    fn fake_invocation(tool_id: &str) -> ToolInvocation {
        ToolInvocation {
            id: ToolInvocationId::new(),
            schema_version: RUNTIME_SCHEMA_VERSION,
            created_at: LedgerTimestamp::now(),
            job_id: crate::domain::JobId::new(),
            repository_id: RepositoryId::new(),
            policy_decision_id: crate::domain::PolicyDecisionId::new(),
            actor: actor(),
            tool_id: tool_id.to_string(),
            requested_capability: "filesystem.read".to_string(),
            args_summary: "test".to_string(),
            resolved_paths: Vec::new(),
            timeout_millis: None,
            redactions: Vec::new(),
        }
    }

    fn request_with_path(path: &str) -> RuntimeJobRequest {
        RuntimeJobRequest::new(actor(), RepositoryId::new(), JobKind::Read, "test goal")
            .with_resource_scope("path", path)
    }

    fn request_with_mutation_path(path: &str) -> RuntimeJobRequest {
        RuntimeJobRequest::new(actor(), RepositoryId::new(), JobKind::Mutate, "test goal")
            .with_resource_scope("path", path)
    }

    fn request_with_artifact_ref_for_repository(
        output_ref_id: &str,
        repository_id: RepositoryId,
    ) -> RuntimeJobRequest {
        RuntimeJobRequest::new(actor(), repository_id, JobKind::Read, "test goal")
            .with_resource_scope("artifact", output_ref_id)
    }

    fn string_value(value: &StructuredValue) -> &str {
        match value {
            StructuredValue::String(value) => value,
            other => panic!("expected String, got {:?}", other),
        }
    }

    fn integer_value(value: &StructuredValue) -> i64 {
        match value {
            StructuredValue::Integer(value) => *value,
            other => panic!("expected Integer, got {:?}", other),
        }
    }

    struct TestEnv {
        root: PathBuf,
    }

    impl TestEnv {
        fn new(name: &str) -> Self {
            let root = unique_test_dir(name);
            Self { root }
        }

        fn create_file(&self, relative: &str, content: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }

        fn create_dir(&self, relative: &str) {
            fs::create_dir_all(self.root.join(relative)).unwrap();
        }

        #[cfg(unix)]
        fn create_symlink(&self, link_relative: &str, target: &Path) {
            let link_path = self.root.join(link_relative);
            if let Some(parent) = link_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            std::os::unix::fs::symlink(target, &link_path).unwrap();
        }

        fn cleanup(&self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // -- Canonicalization tests --

    #[test]
    fn canonicalize_accepts_in_scope_path() {
        let env = TestEnv::new("canon-ok");
        env.create_file("src/main.rs", "fn main() {}");

        let result = canonicalize_within_scope(&env.root, Path::new("src/main.rs"));
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let c = result.unwrap();
        assert!(c.canonical.is_absolute());
        assert_eq!(c.display_path(), "src/main.rs");
        env.cleanup();
    }

    #[test]
    fn canonicalize_rejects_outside_path() {
        let env = TestEnv::new("canon-outside");

        let result = canonicalize_within_scope(&env.root, Path::new("../../etc/passwd"));
        assert!(result.is_err());
        match result.unwrap_err() {
            PathResolutionError::OutsideRepositoryScope { .. } => {}
            other => panic!("expected OutsideRepositoryScope, got {:?}", other),
        }
        env.cleanup();
    }

    #[test]
    fn canonicalize_rejects_nonexistent_target() {
        let env = TestEnv::new("canon-noent");

        let result = canonicalize_within_scope(&env.root, Path::new("no_such_file"));
        assert!(result.is_err());
        match result.unwrap_err() {
            PathResolutionError::TargetNotFound { .. } => {}
            other => panic!("expected TargetNotFound, got {:?}", other),
        }
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_rejects_symlink_escape() {
        let env = TestEnv::new("canon-symlink");
        let outside = unique_test_dir("canon-symlink-outside");
        fs::write(outside.join("secret.txt"), "secret").unwrap();

        env.create_symlink("escape", &outside);

        let result = canonicalize_within_scope(&env.root, Path::new("escape/secret.txt"));
        assert!(result.is_err());
        match result.unwrap_err() {
            PathResolutionError::OutsideRepositoryScope { resolved, root } => {
                assert!(!resolved.starts_with(&root));
            }
            other => panic!("expected OutsideRepositoryScope, got {:?}", other),
        }

        env.cleanup();
        let _ = fs::remove_dir_all(&outside);
    }

    // -- FsListTool tests --

    #[test]
    fn fs_list_succeeds_for_directory() {
        let env = TestEnv::new("list-ok");
        env.create_file("alpha.txt", "a");
        env.create_file("beta.rs", "b");
        env.create_dir("subdir");

        let tool = FsListTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            Some("tool_result.fs.list.v1".to_string()),
            result.schema_ref
        );

        let summary = result.fields.iter().find(|f| f.key == "summary").unwrap();
        assert!(string_value(&summary.value).contains("3 entries"));
        assert!(string_value(&summary.value).contains("2 files"));
        assert!(string_value(&summary.value).contains("1 dirs"));

        let entries = result.fields.iter().find(|f| f.key == "entries").unwrap();
        match &entries.value {
            StructuredValue::StringList(names) => {
                assert_eq!(vec!["alpha.txt", "beta.rs", "subdir"], names.as_slice());
            }
            other => panic!("expected StringList, got {:?}", other),
        }

        // Counts must sum to total entries.
        let fc = result
            .fields
            .iter()
            .find(|f| f.key == "file_count")
            .unwrap();
        let dc = result.fields.iter().find(|f| f.key == "dir_count").unwrap();
        let uc = result
            .fields
            .iter()
            .find(|f| f.key == "unknown_count")
            .unwrap();
        let total: i64 =
            integer_value(&fc.value) + integer_value(&dc.value) + integer_value(&uc.value);
        let entry_count = result
            .fields
            .iter()
            .find(|f| f.key == "entry_count")
            .unwrap();
        assert_eq!(integer_value(&entry_count.value), total);
        assert_eq!(0, integer_value(&uc.value));
        env.cleanup();
    }

    #[test]
    fn fs_list_rejects_out_of_scope() {
        let env = TestEnv::new("list-reject");

        let tool = FsListTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("../../etc");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error_field = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error_field.value).contains("outside repository root"));
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_list_rejects_symlink_escape() {
        let env = TestEnv::new("list-symlink");
        let outside = unique_test_dir("list-symlink-outside");
        fs::create_dir_all(outside.join("leaked")).unwrap();

        env.create_symlink("escape", &outside);

        let tool = FsListTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("escape");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error_field = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error_field.value).contains("outside repository root"));

        env.cleanup();
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn fs_list_classifies_symlinks_as_unknown() {
        let env = TestEnv::new("list-symlink-unknown");
        env.create_file("real.txt", "data");
        env.create_dir("real_dir");
        env.create_symlink("link_to_file", &env.root.join("real.txt"));
        env.create_symlink("link_to_dir", &env.root.join("real_dir"));

        let tool = FsListTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);

        let fc = result
            .fields
            .iter()
            .find(|f| f.key == "file_count")
            .unwrap();
        let dc = result.fields.iter().find(|f| f.key == "dir_count").unwrap();
        let uc = result
            .fields
            .iter()
            .find(|f| f.key == "unknown_count")
            .unwrap();
        assert_eq!(1, integer_value(&fc.value)); // real.txt
        assert_eq!(1, integer_value(&dc.value)); // real_dir
        assert_eq!(2, integer_value(&uc.value)); // link_to_file + link_to_dir

        let summary = result.fields.iter().find(|f| f.key == "summary").unwrap();
        assert!(string_value(&summary.value).contains("4 entries"));
        assert!(string_value(&summary.value).contains("2 other"));

        // Invariant: file_count + dir_count + unknown_count == entry_count
        let entry_count = result
            .fields
            .iter()
            .find(|f| f.key == "entry_count")
            .unwrap();
        let total: i64 =
            integer_value(&fc.value) + integer_value(&dc.value) + integer_value(&uc.value);
        assert_eq!(integer_value(&entry_count.value), total);

        env.cleanup();
    }

    // -- FsStatTool tests --

    #[test]
    fn fs_stat_succeeds_for_file() {
        let env = TestEnv::new("stat-ok");
        env.create_file("data.bin", "hello world");

        let tool = FsStatTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("data.bin");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            Some("tool_result.fs.stat.v1".to_string()),
            result.schema_ref
        );

        let ft = result.fields.iter().find(|f| f.key == "file_type").unwrap();
        assert_eq!(StructuredValue::String("file".to_string()), ft.value);

        let size = result
            .fields
            .iter()
            .find(|f| f.key == "size_bytes")
            .unwrap();
        assert_eq!(StructuredValue::Integer(11), size.value);
        env.cleanup();
    }

    #[test]
    fn fs_stat_rejects_out_of_scope() {
        let env = TestEnv::new("stat-reject");

        let tool = FsStatTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("/etc/passwd");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_stat_rejects_symlink_escape() {
        let env = TestEnv::new("stat-symlink");
        let outside = unique_test_dir("stat-symlink-outside");
        fs::write(outside.join("leaked.txt"), "secret").unwrap();

        env.create_symlink("escape.txt", &outside.join("leaked.txt"));

        let tool = FsStatTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("escape.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);

        env.cleanup();
        let _ = fs::remove_dir_all(&outside);
    }

    // -- FsReadTool tests --

    #[test]
    fn fs_read_reads_text_file() {
        let env = TestEnv::new("read-ok");
        env.create_file("notes.txt", "alpha\nbeta\ngamma");

        let tool = FsReadTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            Some("tool_result.fs.read.v1".to_string()),
            result.schema_ref
        );
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("alpha\nbeta\ngamma", string_value(&content.value));
        let line_count = result
            .fields
            .iter()
            .find(|f| f.key == "line_count")
            .unwrap();
        assert_eq!(3, integer_value(&line_count.value));
        assert!(result.truncation.is_none());
        env.cleanup();
    }

    #[test]
    fn fs_read_supports_line_windows_and_line_numbers() {
        let env = TestEnv::new("read-window");
        env.create_file("notes.txt", "alpha\nbeta\ngamma\ndelta");

        let tool = FsReadTool::new(&env.root)
            .with_window(2, 2)
            .with_line_numbers();
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("2: beta\n3: gamma", string_value(&content.value));
        let end_line = result.fields.iter().find(|f| f.key == "end_line").unwrap();
        assert_eq!(3, integer_value(&end_line.value));
        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("window starts at line 2"));
        assert!(trunc.reason.contains("line limit 2"));
        assert!(trunc.retained_bytes <= trunc.original_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_applies_character_limit() {
        let env = TestEnv::new("read-chars");
        env.create_file("long.txt", "abcdef\nghijkl");

        let tool = FsReadTool::new(&env.root).with_max_chars(4);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("long.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("abcd", string_value(&content.value));
        assert!(result
            .truncation
            .unwrap()
            .reason
            .contains("character limit 4"));
        env.cleanup();
    }

    #[test]
    fn fs_read_applies_byte_limit() {
        let env = TestEnv::new("read-bytes");
        env.create_file("bytes.txt", "abcdefg");

        let tool = FsReadTool::new(&env.root).with_max_bytes(4);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("bytes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("abcd", string_value(&content.value));

        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("byte limit 4"));
        assert_eq!(4, trunc.retained_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_applies_byte_limit_with_multibyte_text() {
        let env = TestEnv::new("read-bytes-multibyte");
        env.create_file("bytes.txt", "ééé");

        let tool = FsReadTool::new(&env.root).with_max_bytes(5);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("bytes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("éé", string_value(&content.value));

        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("byte limit 5"));
        assert_eq!(4, trunc.retained_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_applies_byte_limit_with_line_numbers() {
        let env = TestEnv::new("read-bytes-line-numbers");
        env.create_file("bytes.txt", "abcdef");

        let tool = FsReadTool::new(&env.root)
            .with_max_bytes(8)
            .with_line_numbers();
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("bytes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("1: abcde", string_value(&content.value));

        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("byte limit 8"));
        assert_eq!(5, trunc.retained_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_rejects_missing_path() {
        let env = TestEnv::new("read-missing");
        let tool = FsReadTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("missing.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("not found"));
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_read_rejects_symlink_escape() {
        let env = TestEnv::new("read-symlink");
        let outside = unique_test_dir("read-symlink-outside");
        fs::write(outside.join("secret.txt"), "secret").unwrap();

        env.create_symlink("escape.txt", &outside.join("secret.txt"));

        let tool = FsReadTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("escape.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("outside repository root"));

        env.cleanup();
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn fs_read_rejects_binary_file() {
        let env = TestEnv::new("read-binary");
        let path = env.root.join("binary.bin");
        fs::write(&path, vec![0x61, 0x00, 0x62]).unwrap();

        let tool = FsReadTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("binary.bin");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("binary file rejected"));
        env.cleanup();
    }

    #[test]
    fn fs_read_applies_scan_limit_before_allocating_unbounded_lines() {
        let env = TestEnv::new("read-scan");
        env.create_file("long-line.txt", &"x".repeat(1024));

        let tool = FsReadTool::new(&env.root)
            .with_max_chars(16)
            .with_max_scan_bytes(32);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("long-line.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!(16, string_value(&content.value).len());
        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("character limit 16"));
        assert!(trunc.reason.contains("scan byte limit 32"));
        assert_eq!(16, trunc.retained_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_scan_limit_preserves_utf8_boundaries() {
        let env = TestEnv::new("read-utf8-scan");
        env.create_file("utf8.txt", "ééé");

        let tool = FsReadTool::new(&env.root)
            .with_max_chars(16)
            .with_max_scan_bytes(3);
        let invocation = fake_invocation(tool.tool_id());
        let result = tool.execute(&invocation, &request_with_path("utf8.txt"));

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("é", string_value(&content.value));
        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("scan byte limit 3"));
        assert_eq!(2, trunc.retained_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_exact_scan_limit_at_eof_is_not_truncated() {
        let env = TestEnv::new("read-scan-eof");
        env.create_file("small.txt", "abc");

        let tool = FsReadTool::new(&env.root).with_max_scan_bytes(3);
        let invocation = fake_invocation(tool.tool_id());
        let result = tool.execute(&invocation, &request_with_path("small.txt"));

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("abc", string_value(&content.value));
        assert!(result.truncation.is_none());
        env.cleanup();
    }

    #[test]
    fn fs_read_exact_line_limit_at_eof_is_not_truncated() {
        let env = TestEnv::new("read-lines-eof");
        env.create_file("two.txt", "a\nb");

        let tool = FsReadTool::new(&env.root).with_window(1, 2);
        let invocation = fake_invocation(tool.tool_id());
        let result = tool.execute(&invocation, &request_with_path("two.txt"));

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("a\nb", string_value(&content.value));
        assert!(result.truncation.is_none());
        env.cleanup();
    }

    #[test]
    fn fs_read_exact_character_limit_at_eof_is_not_truncated() {
        let env = TestEnv::new("read-chars-eof");
        env.create_file("small.txt", "abcd");

        let tool = FsReadTool::new(&env.root).with_max_chars(4);
        let invocation = fake_invocation(tool.tool_id());
        let result = tool.execute(&invocation, &request_with_path("small.txt"));

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("abcd", string_value(&content.value));
        assert!(result.truncation.is_none());
        env.cleanup();
    }

    #[test]
    fn fs_read_retained_source_bytes_track_crlf_newlines() {
        let env = TestEnv::new("read-crlf");
        env.create_file("crlf.txt", "a\r\nb");

        let file = File::open(env.root.join("crlf.txt")).unwrap();
        let window = read_text_window(file, 1, 2, 100, None, 1024, true).unwrap();

        assert_eq!("1: a\n2: b", window.content);
        assert_eq!(4, window.retained_source_bytes);
        env.cleanup();
    }

    #[test]
    fn fs_read_args_summary_includes_only_non_default_limits() {
        let tool = FsReadTool::new("/repo")
            .with_window(2, 4)
            .with_max_chars(128)
            .with_max_scan_bytes(256)
            .with_max_file_bytes(512)
            .with_line_numbers();
        let request = request_with_path("artifact.txt");

        assert_eq!(
            "path=artifact.txt window=2:4 chars=128 scan=256 file=512 ln=true",
            tool.args_summary(&request)
        );
    }

    #[test]
    fn fs_read_rejects_out_of_scope_and_directories() {
        let env = TestEnv::new("read-reject");
        env.create_dir("src");

        let tool = FsReadTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let outside = tool.execute(&invocation, &request_with_path("/etc/passwd"));
        assert_eq!(ToolResultStatus::Failed, outside.status);

        let directory = tool.execute(&invocation, &request_with_path("src"));
        assert_eq!(ToolResultStatus::Failed, directory.status);
        env.cleanup();
    }

    #[test]
    fn fs_read_respects_optional_file_size_limit() {
        let env = TestEnv::new("read-size-limit");
        env.create_file("large.txt", "large enough");

        let tool = FsReadTool::new(&env.root).with_max_file_bytes(4);
        let invocation = fake_invocation(tool.tool_id());
        let result = tool.execute(&invocation, &request_with_path("large.txt"));

        assert_eq!(ToolResultStatus::Failed, result.status);
        env.cleanup();
    }

    #[test]
    fn fs_read_reads_repository_scoped_artifact() {
        let env = TestEnv::new("read-artifact");
        let repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));
        let output_ref = artifact_store
            .write_bytes(
                repository_id.as_str(),
                "artifact-read",
                "text/plain; charset=utf-8",
                b"artifact content\nline 2",
            )
            .unwrap();

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let invocation = fake_invocation(tool.tool_id());
        let request =
            request_with_artifact_ref_for_repository(output_ref.id.as_str(), repository_id);
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert!(result
            .fields
            .iter()
            .find(|f| f.key == "content")
            .map(|field| string_value(&field.value))
            .unwrap()
            .starts_with("artifact content"));
        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    #[test]
    fn fs_read_rejects_project_scoped_artifact_without_context() {
        let env = TestEnv::new("read-artifact-project-scoped");
        let repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store-project-scoped");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));
        let output_ref = artifact_store
            .write_bytes_with_metadata(
                repository_id.as_str(),
                "artifact-read",
                "text/plain; charset=utf-8",
                b"artifact content\nline 2",
                ArtifactWriteMetadata {
                    project_id: Some("project-1".to_string()),
                    repository_id: Some(repository_id.as_str().to_string()),
                    original_bytes: None,
                    retained_bytes: None,
                },
            )
            .unwrap();

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let invocation = fake_invocation(tool.tool_id());
        let request =
            request_with_artifact_ref_for_repository(output_ref.id.as_str(), repository_id);
        let denied = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, denied.status);
        let error = denied.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(
            string_value(&error.value).contains("project/job context"),
            "unexpected error: {error:?}"
        );
        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    #[cfg(windows)]
    #[test]
    fn fs_read_rejects_symlink_artifact_on_windows() {
        let env = TestEnv::new("read-artifact-symlink");
        let repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store-symlink");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));
        let output_ref = artifact_store
            .write_bytes(
                repository_id.as_str(),
                "artifact-read",
                "text/plain; charset=utf-8",
                b"artifact content\nline 2",
            )
            .unwrap();

        let link_path = PathBuf::from(&output_ref.uri);
        let target_path = env.root.join("replacement.txt");
        fs::write(&target_path, b"replacement").unwrap();
        fs::remove_file(&link_path).unwrap();

        if let Err(error) = std::os::windows::fs::symlink_file(&target_path, &link_path) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create symlink for test: {error}");
        }

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let invocation = fake_invocation(tool.tool_id());
        let request =
            request_with_artifact_ref_for_repository(output_ref.id.as_str(), repository_id);
        let denied = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, denied.status);
        let error = denied.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("best-effort symlink-blocked"));
        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    #[test]
    fn fs_read_denies_artifact_ref_from_other_repository() {
        let env = TestEnv::new("read-artifact-cross-repo-denial");
        let repository_id = RepositoryId::new();
        let other_repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store-cross-repo");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));
        let output_ref = artifact_store
            .write_bytes(
                repository_id.as_str(),
                "artifact-read",
                "text/plain; charset=utf-8",
                b"artifact content\nline 2",
            )
            .unwrap();

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let invocation = fake_invocation(tool.tool_id());
        let request =
            request_with_artifact_ref_for_repository(output_ref.id.as_str(), repository_id.clone());
        let allowed = tool.execute(&invocation, &request);
        assert_eq!(ToolResultStatus::Succeeded, allowed.status);

        let request =
            request_with_artifact_ref_for_repository(output_ref.id.as_str(), other_repository_id);
        let denied = tool.execute(&invocation, &request);
        assert_eq!(ToolResultStatus::Failed, denied.status);
        let error = denied.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value)
            .contains("artifact is not accessible from this repository context"));

        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    #[test]
    fn fs_read_artifact_ref_denied_does_not_read_repository_file() {
        let env = TestEnv::new("read-artifact-ref-denied-repo-fallback");
        let repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store-denied-fallback");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));
        let output_ref = artifact_store
            .write_bytes_with_metadata(
                repository_id.as_str(),
                "artifact-read",
                "text/plain; charset=utf-8",
                b"artifact content\nline 2",
                ArtifactWriteMetadata {
                    project_id: Some("project-1".to_string()),
                    repository_id: Some(repository_id.as_str().to_string()),
                    original_bytes: None,
                    retained_bytes: None,
                },
            )
            .unwrap();
        env.create_file(
            output_ref.id.as_str(),
            "repository artifact fallback content",
        );

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let request = RuntimeJobRequest::new(actor(), repository_id, JobKind::Read, "test goal")
            .with_resource_scope("artifact_ref", output_ref.id.as_str());
        let invocation = ToolInvocation {
            resolved_paths: tool.resolved_paths(&request),
            ..fake_invocation(tool.tool_id())
        };
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        assert!(invocation.resolved_paths.is_empty());
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(
            string_value(&error.value).contains("project/job context"),
            "unexpected error: {error:?}"
        );
        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    #[test]
    fn fs_read_artifact_ref_missing_does_not_read_repository_file() {
        let env = TestEnv::new("read-artifact-ref-missing-repo-fallback");
        let repository_id = RepositoryId::new();
        let artifact_store_root = unique_test_dir("read-artifact-store-missing-fallback");
        let artifact_store =
            LocalArtifactStore::new(ArtifactStoreConfig::new(artifact_store_root.clone()));

        let output_ref_id = OutputRefId::new();
        env.create_file(
            output_ref_id.as_str(),
            "repository artifact fallback content",
        );

        let tool = FsReadTool::new(&env.root).with_artifact_store(artifact_store);
        let request = RuntimeJobRequest::new(actor(), repository_id, JobKind::Read, "test goal")
            .with_resource_scope("artifact_ref", output_ref_id.as_str());
        let invocation = ToolInvocation {
            resolved_paths: tool.resolved_paths(&request),
            ..fake_invocation(tool.tool_id())
        };
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        assert!(invocation.resolved_paths.is_empty());
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(
            string_value(&error.value).contains("artifact not found"),
            "unexpected error: {error:?}"
        );
        env.cleanup();
        let _ = fs::remove_dir_all(&artifact_store_root);
    }

    // -- FsSearchTool tests --

    #[test]
    fn fs_search_finds_matching_lines() {
        let env = TestEnv::new("search-ok");
        env.create_file("a.txt", "hello world\nfoo bar\nhello again");
        env.create_file("b.txt", "no match here");
        env.create_dir("empty");

        let tool = FsSearchTool::new(&env.root, "hello");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);

        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(StructuredValue::Integer(2), count.value);

        let matches_field = result.fields.iter().find(|f| f.key == "matches").unwrap();
        match &matches_field.value {
            StructuredValue::StringList(list) => {
                assert_eq!(2, list.len());
                assert!(list[0].contains("a.txt:1"));
                assert!(list[1].contains("a.txt:3"));
            }
            other => panic!("expected StringList, got {:?}", other),
        }
        env.cleanup();
    }

    #[test]
    fn fs_search_rejects_out_of_scope() {
        let env = TestEnv::new("search-reject");

        let tool = FsSearchTool::new(&env.root, "anything");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("../../../tmp");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        env.cleanup();
    }

    #[test]
    fn fs_search_truncates_at_max_results() {
        let env = TestEnv::new("search-trunc");
        let mut content = String::new();
        for i in 0..20 {
            content.push_str(&format!("line {} has MATCH here\n", i));
        }
        env.create_file("big.txt", &content);

        let tool = FsSearchTool::new(&env.root, "MATCH").with_max_results(5);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);

        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(StructuredValue::Integer(5), count.value);

        assert!(result.truncation.is_some());
        let trunc = result.truncation.unwrap();
        assert!(trunc.reason.contains("truncated at 5 matches"));

        // retained_bytes must equal the sum of match string byte lengths
        let matches_field = result.fields.iter().find(|f| f.key == "matches").unwrap();
        if let StructuredValue::StringList(ref list) = matches_field.value {
            let expected: u64 = list.iter().map(|s| s.len() as u64).sum();
            assert_eq!(expected, trunc.retained_bytes);
        }
        assert!(
            trunc.retained_bytes <= trunc.original_bytes,
            "retained_bytes ({}) should not exceed original_bytes ({})",
            trunc.retained_bytes,
            trunc.original_bytes,
        );
        env.cleanup();
    }

    #[test]
    fn fs_search_single_file_target() {
        let env = TestEnv::new("search-file");
        env.create_file("target.txt", "find this\nnope\nfind that");

        let tool = FsSearchTool::new(&env.root, "find");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("target.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(StructuredValue::Integer(2), count.value);
        env.cleanup();
    }

    #[test]
    fn fs_search_single_file_respects_max_file_bytes() {
        let env = TestEnv::new("search-large-file");
        env.create_file(
            "large.txt",
            "MATCH in a file that is intentionally too large",
        );

        let tool = FsSearchTool::new(&env.root, "MATCH").with_max_file_bytes(4);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("large.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(0, integer_value(&count.value));
        let skipped = result
            .fields
            .iter()
            .find(|f| f.key == "files_skipped")
            .unwrap();
        assert_eq!(1, integer_value(&skipped.value));
        env.cleanup();
    }

    #[test]
    fn fs_search_skips_binary_files_during_directory_search() {
        let env = TestEnv::new("search-binary");
        env.create_file("good.txt", "needle in text");
        fs::write(
            env.root.join("binary.bin"),
            [0xff, 0xfe, b'n', b'e', b'e', b'd'],
        )
        .unwrap();

        let tool = FsSearchTool::new(&env.root, "needle");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(1, integer_value(&count.value));
        let skipped = result
            .fields
            .iter()
            .find(|f| f.key == "files_skipped")
            .unwrap();
        assert_eq!(1, integer_value(&skipped.value));
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_search_follows_safe_in_scope_symlink() {
        let env = TestEnv::new("search-safe-symlink");
        env.create_file("real/target.txt", "hello through link");
        env.create_symlink("linked", &env.root.join("real"));

        let tool = FsSearchTool::new(&env.root, "hello");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("linked");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(1, integer_value(&count.value));
        env.cleanup();
    }

    // -- FsWriteTool tests --

    #[test]
    fn fs_write_creates_text_file_within_scope() {
        let env = TestEnv::new("write-create");

        let tool = FsWriteTool::new(&env.root, "hello\nworld");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            "hello\nworld",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        let created = result.fields.iter().find(|f| f.key == "created").unwrap();
        assert_eq!(StructuredValue::Bool(true), created.value);
        env.cleanup();
    }

    #[test]
    fn fs_write_rejects_overwrite_without_permission() {
        let env = TestEnv::new("write-overwrite-blocked");
        env.create_file("notes.txt", "original");

        let tool = FsWriteTool::new(&env.root, "replacement");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        assert_eq!(
            "original",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("overwrite disabled"));
        env.cleanup();
    }

    #[test]
    fn fs_write_overwrites_existing_file_when_allowed() {
        let env = TestEnv::new("write-overwrite");
        env.create_file("notes.txt", "original");

        let tool = FsWriteTool::new(&env.root, "replacement")
            .with_allow_overwrite(true)
            .with_allow_create(false);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            "replacement",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        let overwritten = result
            .fields
            .iter()
            .find(|f| f.key == "overwritten")
            .unwrap();
        assert_eq!(StructuredValue::Bool(true), overwritten.value);
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_write_fails_atomically_if_rename_does_not_run() {
        let env = TestEnv::new("write-fails-atomically");
        env.create_file("notes.txt", "original");

        let path = env.root.join("notes.txt");
        let err =
            write_file_bytes_atomically_inner(&path, b"replacement", false, true).unwrap_err();
        assert_eq!(io::ErrorKind::Other, err.kind());
        assert_eq!("original", fs::read_to_string(&path).unwrap());

        let has_temp_file = fs::read_dir(&env.root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(WRITE_FILE_TMP_PREFIX)
            });
        assert!(
            !has_temp_file,
            "temporary write file should be cleaned up on failure"
        );

        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_write_create_fails_atomically_if_destination_appears() {
        let env = TestEnv::new("write-create-fails-atomically");
        let path = env.root.join("notes.txt");

        let err = write_file_bytes_atomically_inner(&path, b"replacement", true, true).unwrap_err();
        assert_eq!(io::ErrorKind::Other, err.kind());
        assert!(!path.exists());

        let has_temp_file = fs::read_dir(&env.root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(WRITE_FILE_TMP_PREFIX)
            });
        assert!(
            !has_temp_file,
            "temporary write file should be cleaned up on failure"
        );

        env.cleanup();
    }

    #[test]
    fn fs_write_rejects_byte_limit_overrun() {
        let env = TestEnv::new("write-limit");

        let tool = FsWriteTool::new(&env.root, "too long").with_max_bytes(4);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        assert!(!env.root.join("notes.txt").exists());
        env.cleanup();
    }

    #[test]
    fn fs_write_rejects_out_of_scope_target() {
        let env = TestEnv::new("write-reject");

        let tool = FsWriteTool::new(&env.root, "hello");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("../../etc/passwd");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("outside repository root"));
        env.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn fs_write_rejects_parent_directory_swap_after_scope_validation() {
        let env = TestEnv::new("write-parent-swap");
        let outside = unique_test_dir("write-parent-swap-outside");
        let backup_path = env.root.join("notes_backup");

        env.create_file("notes/note.txt", "inside");
        fs::create_dir_all(outside.join("notes")).unwrap();
        fs::write(outside.join("notes").join("note.txt"), "outside").unwrap();

        let target = resolve_mutation_target(&env.root, Path::new("notes/note.txt")).unwrap();

        let parent = env.root.join("notes");
        fs::rename(&parent, &backup_path).unwrap();
        std::os::unix::fs::symlink(outside.join("notes"), &parent).unwrap();

        let open_result = open_write_file_no_follow(target.path(), false);
        assert!(open_result.is_err());

        assert_eq!(
            "outside",
            fs::read_to_string(outside.join("notes").join("note.txt")).unwrap()
        );
        assert_eq!(
            "inside",
            fs::read_to_string(backup_path.join("note.txt")).unwrap()
        );

        env.cleanup();
        let _ = fs::remove_dir_all(&outside);
    }

    // -- FsPatchTool tests --

    #[test]
    fn fs_patch_applies_single_exact_replacement() {
        let env = TestEnv::new("patch-exact");
        env.create_file("notes.txt", "alpha\nbeta\ngamma");

        let tool = FsPatchTool::new(&env.root, "beta", "delta");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Succeeded, result.status);
        assert_eq!(
            "alpha\ndelta\ngamma",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        let changed = result.fields.iter().find(|f| f.key == "changed").unwrap();
        assert_eq!(StructuredValue::Bool(true), changed.value);
        env.cleanup();
    }

    #[test]
    fn fs_patch_rejects_ambiguous_match() {
        let env = TestEnv::new("patch-ambiguous");
        env.create_file("notes.txt", "beta\nbeta");

        let tool = FsPatchTool::new(&env.root, "beta", "delta");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("matched 2 times"));
        env.cleanup();
    }

    #[test]
    fn fs_patch_rejects_overlapping_match() {
        let env = TestEnv::new("patch-overlapping");
        env.create_file("notes.txt", "aaa");

        let tool = FsPatchTool::new(&env.root, "aa", "b");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("matched 2 times"));
        assert_eq!(
            "aaa",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        env.cleanup();
    }

    #[test]
    fn fs_patch_rejects_missing_match() {
        let env = TestEnv::new("patch-missing");
        env.create_file("notes.txt", "alpha");

        let tool = FsPatchTool::new(&env.root, "beta", "delta");
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        let error = result.fields.iter().find(|f| f.key == "error").unwrap();
        assert!(string_value(&error.value).contains("was not found"));
        env.cleanup();
    }

    #[test]
    fn fs_patch_rejects_byte_limit_overrun() {
        let env = TestEnv::new("patch-limit");
        env.create_file("notes.txt", "alpha\nbeta");

        let tool = FsPatchTool::new(&env.root, "beta", "replacement").with_max_bytes(8);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_mutation_path("notes.txt");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);
        assert_eq!(
            "alpha\nbeta",
            fs::read_to_string(env.root.join("notes.txt")).unwrap()
        );
        env.cleanup();
    }

    #[test]
    fn fs_patch_read_rejects_content_exceeding_byte_cap() {
        let env = TestEnv::new("patch-read-limit");
        let large_path = env.root.join("notes.txt");
        env.create_file("notes.txt", &"a".repeat(1200));

        let err = read_entire_text_file(&large_path, 1024).unwrap_err();
        assert_eq!(io::ErrorKind::FileTooLarge, err.kind());
        env.cleanup();
    }

    // -- Rendering compatibility --

    #[test]
    fn read_tool_results_render_in_all_formats() {
        let env = TestEnv::new("render");
        env.create_file("hello.txt", "hello");

        let tool = FsListTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path(".");
        let result = tool.execute(&invocation, &request);

        // TOON
        let toon = render_tool_result(&result, &RenderOptions::default()).unwrap();
        assert!(toon.body.contains("status succeeded"));
        assert!(toon.body.contains("tool_id fs.list"));
        assert!(toon.body.contains("fields["));
        assert!(toon.body.contains("  summary,"));

        // Text
        let text = render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();
        assert!(text.body.starts_with("fs.list succeeded:"));

        // JSON
        let json = render_tool_result(&result, &RenderOptions::new(OutputFormat::Json)).unwrap();
        assert!(json.body.contains("\"tool_id\": \"fs.list\""));
        assert!(json.body.contains("\"status\": \"succeeded\""));
        env.cleanup();
    }

    #[test]
    fn failed_tool_result_renders_correctly() {
        let env = TestEnv::new("render-fail");
        let tool = FsStatTool::new(&env.root);
        let invocation = fake_invocation(tool.tool_id());
        let request = request_with_path("/etc/passwd");
        let result = tool.execute(&invocation, &request);

        assert_eq!(ToolResultStatus::Failed, result.status);

        let text = render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();
        assert!(text.body.contains("fs.stat failed:"));
        env.cleanup();
    }

    // -- Runtime integration --

    #[test]
    fn fs_list_integrates_with_secretary_runtime() {
        let env = TestEnv::new("runtime");
        env.create_file("readme.md", "# hello");
        env.create_dir("src");

        let runtime = SecretaryRuntime::in_memory();
        let repository = RepositoryRecord::new(
            "test-repo",
            env.root.to_string_lossy(),
            RepositoryTrustState::Trusted,
            LedgerTimestamp::now(),
        );
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let tool = FsListTool::new(&env.root);
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "list repository root",
        );

        let receipt = runtime.run_tool_job(request, &tool).unwrap();

        assert_eq!(crate::domain::JobStatus::Succeeded, receipt.job.status);
        assert_eq!(
            crate::domain::PolicyOutcome::Allowed,
            receipt.policy_decision.outcome
        );
        assert!(receipt.tool_invocation.is_some());
        assert!(!receipt
            .tool_invocation
            .as_ref()
            .unwrap()
            .resolved_paths
            .is_empty());
        assert!(receipt.tool_result.is_some());
        assert!(receipt.audit_record.is_some());
        assert!(receipt.rendered_output.is_some());
        assert!(receipt
            .rendered_output
            .as_ref()
            .unwrap()
            .body
            .contains("entries"));
        env.cleanup();
    }

    #[test]
    fn fs_read_integrates_with_secretary_runtime() {
        let env = TestEnv::new("runtime-read");
        env.create_file("readme.md", "# hello\nbody");

        let runtime = SecretaryRuntime::in_memory();
        let repository = RepositoryRecord::new(
            "test-repo",
            env.root.to_string_lossy(),
            RepositoryTrustState::Trusted,
            LedgerTimestamp::now(),
        );
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let tool = FsReadTool::new(&env.root);
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "read repository file",
        )
        .with_resource_scope("path", "readme.md");

        let receipt = runtime.run_tool_job(request, &tool).unwrap();

        assert_eq!(crate::domain::JobStatus::Succeeded, receipt.job.status);
        assert_eq!("fs.read", receipt.tool_invocation.as_ref().unwrap().tool_id);
        assert!(!receipt
            .tool_invocation
            .as_ref()
            .unwrap()
            .resolved_paths
            .is_empty());
        let result = receipt.tool_result.unwrap();
        let content = result.fields.iter().find(|f| f.key == "content").unwrap();
        assert_eq!("# hello\nbody", string_value(&content.value));
        env.cleanup();
    }

    #[test]
    fn fs_search_integrates_with_secretary_runtime() {
        let env = TestEnv::new("runtime-search");
        env.create_file("greeting.txt", "hello world\nhello universe");

        let runtime = SecretaryRuntime::in_memory();
        let repository = RepositoryRecord::new(
            "test-repo",
            env.root.to_string_lossy(),
            RepositoryTrustState::Trusted,
            LedgerTimestamp::now(),
        );
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let tool = FsSearchTool::new(&env.root, "hello");
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "search for hello",
        );

        let receipt = runtime.run_tool_job(request, &tool).unwrap();

        assert_eq!(crate::domain::JobStatus::Succeeded, receipt.job.status);
        assert!(receipt.tool_result.is_some());
        let result = receipt.tool_result.unwrap();
        let count = result
            .fields
            .iter()
            .find(|f| f.key == "match_count")
            .unwrap();
        assert_eq!(StructuredValue::Integer(2), count.value);
        env.cleanup();
    }

    #[test]
    fn fs_write_and_patch_integrate_with_secretary_runtime() {
        let env = TestEnv::new("runtime-mutate");

        let runtime = SecretaryRuntime::in_memory();
        let repository = RepositoryRecord::new(
            "test-repo",
            env.root.to_string_lossy(),
            RepositoryTrustState::Trusted,
            LedgerTimestamp::now(),
        );
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let write_tool = FsWriteTool::new(&env.root, "hello")
            .with_allow_create(true)
            .with_allow_overwrite(true);
        let write_request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Mutate,
            "write repository file",
        )
        .with_resource_scope("path", "note.txt")
        .with_requested_capabilities(vec!["filesystem.write".to_string()]);
        let write_receipt = runtime.run_tool_job(write_request, &write_tool).unwrap();
        assert_eq!(
            crate::domain::JobStatus::Succeeded,
            write_receipt.job.status
        );
        assert_eq!(
            "hello",
            fs::read_to_string(env.root.join("note.txt")).unwrap()
        );

        let patch_tool = FsPatchTool::new(&env.root, "hello", "world");
        let patch_request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Mutate,
            "patch repository file",
        )
        .with_resource_scope("path", "note.txt")
        .with_requested_capabilities(vec!["filesystem.patch".to_string()]);
        let patch_receipt = runtime.run_tool_job(patch_request, &patch_tool).unwrap();

        assert_eq!(
            crate::domain::JobStatus::Succeeded,
            patch_receipt.job.status
        );
        assert_eq!(
            "world",
            fs::read_to_string(env.root.join("note.txt")).unwrap()
        );
        env.cleanup();
    }
}
