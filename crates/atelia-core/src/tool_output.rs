//! Rendering helpers for canonical tool results.

use crate::domain::{
    ArtifactRef, OutputRef, RedactionMarker, StructuredValue, ToolResult, ToolResultStatus,
    TruncationMetadata,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

const TOOL_RESULT_SCHEMA_NAME: &str = "tool_result";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Toon,
    Json,
    Text,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toon => "toon",
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderOptions {
    pub format: OutputFormat,
    pub include_policy: bool,
    pub include_diagnostics: bool,
    pub include_cost: bool,
}

impl RenderOptions {
    pub fn new(format: OutputFormat) -> Self {
        Self {
            format,
            ..Self::default()
        }
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Toon,
            include_policy: false,
            include_diagnostics: false,
            include_cost: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedToolOutput {
    pub format: OutputFormat,
    pub schema_version: String,
    pub body: String,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutputRenderError {
    UnsupportedFormat { format: OutputFormat },
    JsonSerialize { reason: String },
}

impl fmt::Display for ToolOutputRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat { format } => {
                write!(f, "tool output format {} is not supported", format.as_str())
            }
            Self::JsonSerialize { reason } => {
                write!(f, "tool output json serialization failed: {reason}")
            }
        }
    }
}

impl Error for ToolOutputRenderError {}

pub fn render_tool_result(
    result: &ToolResult,
    options: &RenderOptions,
) -> Result<RenderedToolOutput, ToolOutputRenderError> {
    let schema_version = schema_version(result);

    match options.format {
        OutputFormat::Toon => Ok(RenderedToolOutput {
            format: OutputFormat::Toon,
            schema_version,
            body: render_toon(result, options, None),
            fallback_reason: None,
        }),
        OutputFormat::Text => Ok(RenderedToolOutput {
            format: OutputFormat::Text,
            schema_version,
            body: render_text(result, options),
            fallback_reason: None,
        }),
        OutputFormat::Json => Ok(RenderedToolOutput {
            format: OutputFormat::Json,
            schema_version,
            body: serde_json::to_string_pretty(&renderable_result(result, options)).map_err(
                |error| ToolOutputRenderError::JsonSerialize {
                    reason: error.to_string(),
                },
            )?,
            fallback_reason: None,
        }),
    }
}

fn render_toon(
    result: &ToolResult,
    options: &RenderOptions,
    fallback_reason: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    let result = renderable_result(result, options);

    lines.push(format!("status {}", status_name(result.status)));
    lines.push(format!("schema_version {}", schema_version(&result)));
    lines.push(format!("format {}", OutputFormat::Toon.as_str()));

    if let Some(reason) = fallback_reason {
        lines.push(format!(
            "requested_format {}",
            render_toon_value(OutputFormat::Json.as_str())
        ));
        lines.push(format!(
            "format_fallback_reason {}",
            render_toon_value(reason)
        ));
    }

    lines.push(format!("tool_id {}", render_toon_value(&result.tool_id)));
    lines.push(format!("result_id {}", result.id.as_str()));
    lines.push(format!("invocation_id {}", result.invocation_id.as_str()));

    if let Some(schema_ref) = &result.schema_ref {
        lines.push(format!("schema_ref {}", render_toon_value(schema_ref)));
    }

    append_fields(&mut lines, &result);
    append_artifact_refs(&mut lines, "evidence_refs", &result.evidence_refs);
    append_output_refs(&mut lines, &result.output_refs);

    if let Some(truncation) = &result.truncation {
        append_truncation(&mut lines, truncation);
    }

    append_redactions(&mut lines, &result.redactions);

    lines.join("\n")
}

fn render_text(result: &ToolResult, options: &RenderOptions) -> String {
    let result = renderable_result(result, options);
    let summary = summary_field(&result).unwrap_or("no summary");
    let mut parts = vec![format!(
        "{} {}: {}",
        result.tool_id,
        status_name(result.status),
        summary
    )];

    if !result.fields.is_empty() {
        parts.push(format!("{} field(s)", result.fields.len()));
    }

    if !result.evidence_refs.is_empty() {
        parts.push(format!("{} evidence ref(s)", result.evidence_refs.len()));
    }

    if !result.output_refs.is_empty() {
        parts.push(format!("{} output ref(s)", result.output_refs.len()));
    }

    if let Some(truncation) = &result.truncation {
        parts.push(format!(
            "truncated {}/{} bytes: {}",
            truncation.retained_bytes, truncation.original_bytes, truncation.reason
        ));
    }

    if !result.redactions.is_empty() {
        let markers = result
            .redactions
            .iter()
            .map(|redaction| {
                format!(
                    "{} ({}, redacted_at_ms {})",
                    redaction.field_path, redaction.reason, redaction.redacted_at.unix_millis
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        parts.push(format!(
            "{} redaction(s): {}",
            result.redactions.len(),
            markers
        ));
    }

    parts.join("; ")
}

fn renderable_result(result: &ToolResult, options: &RenderOptions) -> ToolResult {
    let mut result = result.clone();
    result
        .fields
        .retain(|field| should_render_field(&field.key, options));
    result
}

fn should_render_field(key: &str, options: &RenderOptions) -> bool {
    match optional_field_channel(key) {
        Some(OptionalFieldChannel::Policy) => options.include_policy,
        Some(OptionalFieldChannel::Diagnostics) => options.include_diagnostics,
        Some(OptionalFieldChannel::Cost) => options.include_cost,
        None => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionalFieldChannel {
    Policy,
    Diagnostics,
    Cost,
}

fn optional_field_channel(key: &str) -> Option<OptionalFieldChannel> {
    let normalized = key
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '.', ' '], "_");

    if normalized == "policy"
        || normalized.starts_with("policy_")
        || matches!(normalized.as_str(), "needs_approval" | "blocked_reason")
    {
        Some(OptionalFieldChannel::Policy)
    } else if normalized == "diagnostic"
        || normalized == "diagnostics"
        || normalized.starts_with("diagnostic_")
        || normalized.starts_with("diagnostics_")
        || matches!(normalized.as_str(), "retry_hint" | "parser_failure")
    {
        Some(OptionalFieldChannel::Diagnostics)
    } else if normalized == "cost"
        || normalized.starts_with("cost_")
        || normalized.ends_with("_tokens")
        || matches!(normalized.as_str(), "duration_ms" | "elapsed_ms")
    {
        Some(OptionalFieldChannel::Cost)
    } else {
        None
    }
}

fn append_fields(lines: &mut Vec<String>, result: &ToolResult) {
    if result.fields.is_empty() {
        return;
    }

    lines.push(format!("fields[{}]{{key,value}}", result.fields.len()));

    for field in &result.fields {
        lines.push(format!(
            "  {},{}",
            render_toon_value(&field.key),
            render_structured_value(&field.value)
        ));
    }
}

fn append_artifact_refs(lines: &mut Vec<String>, label: &str, refs: &[ArtifactRef]) {
    if refs.is_empty() {
        return;
    }

    lines.push(format!(
        "{label}[{}]{{id,uri,media_type,label,digest}}",
        refs.len()
    ));

    for reference in refs {
        lines.push(format!(
            "  {},{},{},{},{}",
            reference.id.as_str(),
            render_toon_value(&reference.uri),
            render_toon_value(&reference.media_type),
            render_optional_string(reference.label.as_deref()),
            render_optional_string(reference.digest.as_deref())
        ));
    }
}

fn append_output_refs(lines: &mut Vec<String>, refs: &[OutputRef]) {
    if refs.is_empty() {
        return;
    }

    lines.push(format!(
        "output_refs[{}]{{id,uri,media_type,label,digest}}",
        refs.len()
    ));

    for reference in refs {
        lines.push(format!(
            "  {},{},{},{},{}",
            reference.id.as_str(),
            render_toon_value(&reference.uri),
            render_toon_value(&reference.media_type),
            render_optional_string(reference.label.as_deref()),
            render_optional_string(reference.digest.as_deref())
        ));
    }
}

fn append_truncation(lines: &mut Vec<String>, truncation: &TruncationMetadata) {
    lines.push("truncation".to_string());
    lines.push("  truncated true".to_string());
    lines.push(format!("  original_bytes {}", truncation.original_bytes));
    lines.push(format!("  retained_bytes {}", truncation.retained_bytes));
    lines.push(format!(
        "  reason {}",
        render_toon_value(&truncation.reason)
    ));
}

fn append_redactions(lines: &mut Vec<String>, redactions: &[RedactionMarker]) {
    if redactions.is_empty() {
        return;
    }

    lines.push(format!(
        "redactions[{}]{{field_path,reason,redacted_at_ms}}",
        redactions.len()
    ));

    for redaction in redactions {
        lines.push(format!(
            "  {},{},{}",
            render_toon_value(&redaction.field_path),
            render_toon_value(&redaction.reason),
            redaction.redacted_at.unix_millis
        ));
    }
}

fn schema_version(result: &ToolResult) -> String {
    format!("{TOOL_RESULT_SCHEMA_NAME}.v{}", result.schema_version)
}

fn summary_field(result: &ToolResult) -> Option<&str> {
    result.fields.iter().find_map(|field| {
        if field.key == "summary" {
            match &field.value {
                StructuredValue::String(value) => Some(value.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

fn status_name(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Succeeded => "succeeded",
        ToolResultStatus::Failed => "failed",
        ToolResultStatus::Canceled => "canceled",
        ToolResultStatus::TimedOut => "timeout",
    }
}

fn render_structured_value(value: &StructuredValue) -> String {
    match value {
        StructuredValue::Null => "null".to_string(),
        StructuredValue::Bool(value) => value.to_string(),
        StructuredValue::Integer(value) => value.to_string(),
        StructuredValue::String(value) => render_toon_value(value),
        StructuredValue::StringList(values) => {
            let values = values
                .iter()
                .map(|value| render_toon_value(value))
                .collect::<Vec<_>>()
                .join(",");

            format!("[{values}]")
        }
    }
}

fn render_optional_string(value: Option<&str>) -> String {
    value
        .map(render_toon_value)
        .unwrap_or_else(|| "null".to_string())
}

fn render_toon_value(value: &str) -> String {
    if value.is_empty() || is_toon_literal(value) || value.chars().any(needs_quotes) {
        quote_string(value)
    } else {
        value.to_string()
    }
}

fn is_toon_literal(value: &str) -> bool {
    matches!(value, "null" | "true" | "false")
        || value.parse::<i64>().is_ok()
        || value.parse::<f64>().is_ok()
}

fn needs_quotes(character: char) -> bool {
    character.is_control()
        || character.is_whitespace()
        || matches!(
            character,
            ',' | '[' | ']' | '{' | '}' | '"' | '\'' | '#' | ':' | '\\'
        )
}

fn quote_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');

    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                quoted.push_str("\\u{");
                quoted.push_str(&format!("{:x}", character as u32));
                quoted.push('}');
            }
            character => quoted.push(character),
        }
    }

    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ArtifactRefId, LedgerTimestamp, OutputRefId, ToolInvocationId, ToolResultField,
        ToolResultId,
    };

    fn sample_result() -> ToolResult {
        ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::from_unix_millis(1_700_000_000_000),
            invocation_id: ToolInvocationId::new(),
            tool_id: "git_status".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("schema:git.status.v1".to_string()),
            fields: vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String("2 modified files".to_string()),
                },
                ToolResultField {
                    key: "changed_files".to_string(),
                    value: StructuredValue::StringList(vec![
                        "crates/atelia-core/src/lib.rs".to_string(),
                        "docs/tool-output-schema.md".to_string(),
                    ]),
                },
                ToolResultField {
                    key: "exit_code".to_string(),
                    value: StructuredValue::Integer(0),
                },
            ],
            evidence_refs: vec![ArtifactRef {
                id: ArtifactRefId::new(),
                uri: "/tmp/evidence.txt".to_string(),
                media_type: "text/plain".to_string(),
                label: Some("status evidence".to_string()),
                digest: Some("sha256:abc123".to_string()),
            }],
            output_refs: vec![OutputRef {
                id: OutputRefId::new(),
                uri: "/tmp/stdout.txt".to_string(),
                media_type: "text/plain".to_string(),
                label: Some("stdout".to_string()),
                digest: None,
            }],
            truncation: None,
            redactions: Vec::new(),
        }
    }

    fn result_with_optional_channels() -> ToolResult {
        let mut result = sample_result();
        result.fields = vec![
            ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("optional channel sample".to_string()),
            },
            ToolResultField {
                key: "policy.state".to_string(),
                value: StructuredValue::String("allowed_with_audit".to_string()),
            },
            ToolResultField {
                key: "diagnostics.parser_failure".to_string(),
                value: StructuredValue::String("none".to_string()),
            },
            ToolResultField {
                key: "cost.output_tokens".to_string(),
                value: StructuredValue::Integer(128),
            },
        ];
        result
    }

    #[test]
    fn toon_rendering_keeps_contract_order_and_content() {
        let result = sample_result();
        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();
        let lines = rendered.body.lines().collect::<Vec<_>>();

        assert_eq!(rendered.format, OutputFormat::Toon);
        assert_eq!(rendered.schema_version, "tool_result.v1");
        assert_eq!(rendered.fallback_reason, None);
        assert_eq!(lines[0], "status succeeded");
        assert_eq!(lines[1], "schema_version tool_result.v1");
        assert_eq!(lines[2], "format toon");
        assert_eq!(lines[3], "tool_id git_status");
        assert_eq!(lines[4], format!("result_id {}", result.id.as_str()));
        assert_eq!(
            lines[5],
            format!("invocation_id {}", result.invocation_id.as_str())
        );
        assert!(rendered
            .body
            .contains("schema_ref \"schema:git.status.v1\""));
        assert!(rendered.body.contains("fields[3]{key,value}"));
        assert!(rendered.body.contains("  summary,\"2 modified files\""));
        assert!(rendered
            .body
            .contains("evidence_refs[1]{id,uri,media_type,label,digest}"));
        assert!(rendered
            .body
            .contains("output_refs[1]{id,uri,media_type,label,digest}"));
    }

    #[test]
    fn text_rendering_is_short_and_readable() {
        let result = sample_result();
        let options = RenderOptions::new(OutputFormat::Text);
        let rendered = render_tool_result(&result, &options).unwrap();

        assert_eq!(rendered.format, OutputFormat::Text);
        assert_eq!(
            rendered.body,
            "git_status succeeded: 2 modified files; 3 field(s); 1 evidence ref(s); 1 output ref(s)"
        );
    }

    #[test]
    fn json_rendering_uses_canonical_result_schema() {
        let result = sample_result();
        let options = RenderOptions::new(OutputFormat::Json);
        let rendered = render_tool_result(&result, &options).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(rendered.fallback_reason, None);
        assert!(rendered.body.contains("\"tool_id\": \"git_status\""));
        assert!(rendered.body.contains("\"status\": \"succeeded\""));
        assert!(rendered.body.contains("\"schema_version\": 1"));
    }

    #[test]
    fn render_options_filter_optional_channels_in_toon() {
        let result = result_with_optional_channels();
        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();

        assert!(rendered.body.contains("fields[1]{key,value}"));
        assert!(rendered
            .body
            .contains("  summary,\"optional channel sample\""));
        assert!(!rendered.body.contains("policy.state"));
        assert!(!rendered.body.contains("diagnostics.parser_failure"));
        assert!(!rendered.body.contains("cost.output_tokens"));

        let options = RenderOptions {
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
            ..RenderOptions::default()
        };
        let rendered = render_tool_result(&result, &options).unwrap();

        assert!(rendered.body.contains("fields[4]{key,value}"));
        assert!(rendered.body.contains("  policy.state,allowed_with_audit"));
        assert!(rendered.body.contains("  diagnostics.parser_failure,none"));
        assert!(rendered.body.contains("  cost.output_tokens,128"));
    }

    #[test]
    fn render_options_filter_optional_channels_in_text() {
        let result = result_with_optional_channels();
        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();

        assert_eq!(
            rendered.body,
            "git_status succeeded: optional channel sample; 1 field(s); 1 evidence ref(s); 1 output ref(s)"
        );

        let options = RenderOptions {
            format: OutputFormat::Text,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        };
        let rendered = render_tool_result(&result, &options).unwrap();

        assert_eq!(
            rendered.body,
            "git_status succeeded: optional channel sample; 4 field(s); 1 evidence ref(s); 1 output ref(s)"
        );
    }

    #[test]
    fn render_options_filter_optional_channels_in_json() {
        let result = result_with_optional_channels();
        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Json)).unwrap();

        assert!(rendered.body.contains("\"key\": \"summary\""));
        assert!(!rendered.body.contains("\"key\": \"policy.state\""));
        assert!(!rendered
            .body
            .contains("\"key\": \"diagnostics.parser_failure\""));
        assert!(!rendered.body.contains("\"key\": \"cost.output_tokens\""));

        let options = RenderOptions {
            format: OutputFormat::Json,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        };
        let rendered = render_tool_result(&result, &options).unwrap();

        assert!(rendered.body.contains("\"key\": \"policy.state\""));
        assert!(rendered
            .body
            .contains("\"key\": \"diagnostics.parser_failure\""));
        assert!(rendered.body.contains("\"key\": \"cost.output_tokens\""));
    }

    #[test]
    fn canceled_status_uses_domain_spelling() {
        let mut result = sample_result();
        result.status = ToolResultStatus::Canceled;

        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();

        assert!(rendered.body.contains("status canceled"));
        assert!(!rendered.body.contains("status cancelled"));
    }

    #[test]
    fn toon_string_literals_are_quoted_to_preserve_types() {
        let mut result = sample_result();
        result.fields = vec![
            ToolResultField {
                key: "literal_null".to_string(),
                value: StructuredValue::String("null".to_string()),
            },
            ToolResultField {
                key: "literal_true".to_string(),
                value: StructuredValue::String("true".to_string()),
            },
            ToolResultField {
                key: "integer_like".to_string(),
                value: StructuredValue::String("42".to_string()),
            },
            ToolResultField {
                key: "float_like".to_string(),
                value: StructuredValue::String("-3.14".to_string()),
            },
            ToolResultField {
                key: "actual_null".to_string(),
                value: StructuredValue::Null,
            },
            ToolResultField {
                key: "actual_bool".to_string(),
                value: StructuredValue::Bool(true),
            },
            ToolResultField {
                key: "actual_integer".to_string(),
                value: StructuredValue::Integer(42),
            },
            ToolResultField {
                key: "literal_list".to_string(),
                value: StructuredValue::StringList(vec![
                    "false".to_string(),
                    "7".to_string(),
                    "plain".to_string(),
                ]),
            },
        ];

        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();

        assert!(rendered.body.contains("  literal_null,\"null\""));
        assert!(rendered.body.contains("  literal_true,\"true\""));
        assert!(rendered.body.contains("  integer_like,\"42\""));
        assert!(rendered.body.contains("  float_like,\"-3.14\""));
        assert!(rendered.body.contains("  actual_null,null"));
        assert!(rendered.body.contains("  actual_bool,true"));
        assert!(rendered.body.contains("  actual_integer,42"));
        assert!(rendered
            .body
            .contains("  literal_list,[\"false\",\"7\",plain]"));
    }

    #[test]
    fn redaction_and_truncation_markers_are_preserved() {
        let mut result = sample_result();
        result.fields.push(ToolResultField {
            key: "secret note".to_string(),
            value: StructuredValue::String("line one\nline two".to_string()),
        });
        result.truncation = Some(TruncationMetadata {
            original_bytes: 4096,
            retained_bytes: 512,
            reason: "artifact threshold".to_string(),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.secret note".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();

        assert!(rendered.body.contains("truncation"));
        assert!(rendered.body.contains("  truncated true"));
        assert!(rendered.body.contains("  original_bytes 4096"));
        assert!(rendered.body.contains("  retained_bytes 512"));
        assert!(rendered.body.contains("  reason \"artifact threshold\""));
        assert!(rendered
            .body
            .contains("redactions[1]{field_path,reason,redacted_at_ms}"));
        assert!(rendered
            .body
            .contains("  \"fields.secret note\",\"policy secret\",1700000000123"));
        assert!(rendered
            .body
            .contains("  \"secret note\",\"line one\\nline two\""));
    }

    #[test]
    fn text_rendering_preserves_redaction_and_truncation_markers() {
        let mut result = sample_result();
        result.truncation = Some(TruncationMetadata {
            original_bytes: 4096,
            retained_bytes: 512,
            reason: "artifact threshold".to_string(),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.secret note".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();

        assert!(rendered
            .body
            .contains("truncated 512/4096 bytes: artifact threshold"));
        assert!(rendered.body.contains(
            "1 redaction(s): fields.secret note (policy secret, redacted_at_ms 1700000000123)"
        ));
    }
}
