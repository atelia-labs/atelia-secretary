//! Built-in filesystem read tools for Atelia Secretary.
//!
//! Provides repository-scoped `fs.list`, `fs.stat`, and `fs.search` tools that
//! implement [`RuntimeTool`] and enforce path canonicalization with symlink
//! escape rejection per `docs/execution-semantics.md`.

use crate::domain::{
    LedgerTimestamp, RedactionMarker, StructuredValue, ToolInvocation, ToolResult, ToolResultField,
    ToolResultId, ToolResultStatus, TruncationMetadata,
};
use crate::runtime::RuntimeJobRequest;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

const TOOLS_SCHEMA_VERSION: u32 = 1;
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

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    dir_count += 1;
                } else {
                    file_count += 1;
                }
            }
            names.push(name);
        }

        names.sort();

        let summary = format!(
            "{} entries in {} ({} files, {} dirs)",
            names.len(),
            canonical.display_path(),
            file_count,
            dir_count,
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
        let mut truncated = false;
        let mut total_bytes: u64 = 0;

        if canonical.canonical.is_file() {
            files_searched = 1;
            let _ = search_file(
                &canonical.canonical,
                &canonical.root,
                &self.pattern,
                self.max_results,
                &mut matches,
                &mut truncated,
                &mut total_bytes,
            );
        } else if canonical.canonical.is_dir() {
            let _ = search_recursive(
                &canonical.canonical,
                &canonical.root,
                &self.pattern,
                self.max_results,
                self.max_file_bytes,
                &mut matches,
                &mut files_searched,
                &mut truncated,
                &mut total_bytes,
            );
        }

        let truncation = if truncated {
            Some(TruncationMetadata {
                original_bytes: total_bytes,
                retained_bytes: total_bytes.min(self.max_results as u64 * 256),
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
    truncated: &mut bool,
    total_bytes: &mut u64,
) -> io::Result<()> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        if *truncated {
            return Ok(());
        }
        let entry = entry?;
        let path = entry.path();

        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !canonical.starts_with(root) {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            search_recursive(
                &canonical,
                root,
                pattern,
                max_results,
                max_file_bytes,
                matches,
                files_searched,
                truncated,
                total_bytes,
            )?;
        } else if file_type.is_file() {
            let metadata = fs::metadata(&canonical)?;
            if metadata.len() > max_file_bytes {
                continue;
            }
            *files_searched += 1;
            let _ = search_file(
                &canonical,
                root,
                pattern,
                max_results,
                matches,
                truncated,
                total_bytes,
            );
        }
    }
    Ok(())
}

fn search_file(
    path: &Path,
    root: &Path,
    pattern: &str,
    max_results: usize,
    matches: &mut Vec<String>,
    truncated: &mut bool,
    total_bytes: &mut u64,
) -> io::Result<()> {
    let file = fs::File::open(path)?;
    *total_bytes += file.metadata()?.len();

    let reader = io::BufReader::new(file);
    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();

    for (line_num, line_result) in reader.lines().enumerate() {
        if matches.len() >= max_results {
            *truncated = true;
            return Ok(());
        }
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.contains(pattern) {
            matches.push(format!("{}:{}", relative, line_num + 1));
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
