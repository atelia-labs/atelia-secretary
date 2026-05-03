//! Built-in filesystem read tools for Atelia Secretary.
//!
//! Provides repository-scoped `fs.list`, `fs.stat`, `fs.read`, and `fs.search`
//! tools that implement [`crate::runtime::RuntimeTool`] and enforce path
//! canonicalization with symlink escape rejection per
//! `docs/execution-semantics.md`.

use crate::domain::{
    LedgerTimestamp, RedactionMarker, ResolvedPath, StructuredValue, ToolInvocation, ToolResult,
    ToolResultField, ToolResultId, ToolResultStatus, TruncationMetadata,
};
use crate::runtime::RuntimeJobRequest;
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};

const TOOLS_SCHEMA_VERSION: u32 = 1;
const DEFAULT_READ_MAX_LINES: usize = 120;
const DEFAULT_READ_MAX_CHARS: usize = 32 * 1024;
const DEFAULT_READ_MAX_SCAN_BYTES: u64 = 1024 * 1024;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 100;
const DEFAULT_SEARCH_MAX_FILE_BYTES: u64 = 64 * 1024;

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
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
    max_scan_bytes: u64,
    max_file_bytes: Option<u64>,
    include_line_numbers: bool,
}

impl FsReadTool {
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
            start_line: 1,
            max_lines: DEFAULT_READ_MAX_LINES,
            max_chars: DEFAULT_READ_MAX_CHARS,
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
        resolved_path_for_request(&self.repository_root, request)
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        let schema_ref = "tool_result.fs.read.v1";
        let relative = target_from_request(request);

        let canonical = match canonicalize_within_scope(&self.repository_root, &relative) {
            Ok(c) => c,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: path rejected".to_string(),
                    err.to_string(),
                );
            }
        };

        if !canonical.canonical.is_file() {
            return failed_result(
                invocation,
                schema_ref,
                "read failed: target is not a file".to_string(),
                canonical.display_path(),
            );
        }

        let metadata = match fs::metadata(&canonical.canonical) {
            Ok(metadata) => metadata,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: cannot read file metadata".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
                );
            }
        };

        if let Some(max_file_bytes) = self.max_file_bytes {
            if metadata.len() > max_file_bytes {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: file exceeds configured byte limit".to_string(),
                    format!(
                        "{} is {} bytes; limit is {} bytes",
                        canonical.display_path(),
                        metadata.len(),
                        max_file_bytes
                    ),
                );
            }
        }

        let window = match read_text_window(
            &canonical.canonical,
            self.start_line,
            self.max_lines,
            self.max_chars,
            self.max_scan_bytes,
            self.include_line_numbers,
        ) {
            Ok(window) => window,
            Err(err) => {
                return failed_result(
                    invocation,
                    schema_ref,
                    "read failed: cannot read UTF-8 text".to_string(),
                    format!("{}: {}", canonical.display_path(), err),
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
            canonical.display_path()
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
    truncated_by_scan: bool,
}

fn read_text_window(
    path: &Path,
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
    max_scan_bytes: u64,
    include_line_numbers: bool,
) -> io::Result<ReadWindow> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let start_line = start_line.max(1);

    let mut content = String::new();
    let mut end_line = 0;
    let mut line_count = 0;
    let mut used_chars = 0;
    let mut retained_source_bytes = 0;
    let mut scanned_bytes = 0;
    let mut previous_retained_newline_bytes = 0;
    let mut truncated_by_lines = false;
    let mut truncated_by_chars = false;
    let mut truncated_by_scan = false;
    let mut current_line = 0;
    let mut line_bytes = Vec::new();

    loop {
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
        if line_count >= max_lines {
            truncated_by_lines = true;
            break;
        }
        if used_chars >= max_chars {
            truncated_by_chars = true;
            break;
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

        if separator_chars == 1 {
            content.push('\n');
            used_chars += 1;
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

        content.push_str(&rendered);
        used_chars += rendered_chars;
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Actor, JobKind, RepositoryId, RepositoryRecord, RepositoryTrustState, ToolInvocationId,
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
    fn fs_read_retained_source_bytes_track_crlf_newlines() {
        let env = TestEnv::new("read-crlf");
        env.create_file("crlf.txt", "a\r\nb");

        let window = read_text_window(&env.root.join("crlf.txt"), 1, 2, 100, 1024, true).unwrap();

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
}
