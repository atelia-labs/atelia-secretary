//! Rendering helpers for canonical tool results.

use crate::domain::{
    ArtifactRef, OutputRef, RedactionMarker, StructuredValue, ToolResult, ToolResultStatus,
    TruncationMetadata,
};
use crate::settings::OversizeOutputPolicy;
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
pub struct ToolOutputRenderPolicy {
    pub render_options: RenderOptions,
    pub max_fields: Option<usize>,
    pub max_inline_lines: Option<usize>,
    pub max_inline_bytes: Option<u64>,
    pub oversize_policy: OversizeOutputPolicy,
    pub include_evidence_refs: bool,
    pub include_output_refs: bool,
    pub include_redactions: bool,
}

impl ToolOutputRenderPolicy {
    pub fn from_render_options(render_options: RenderOptions) -> Self {
        Self {
            render_options,
            max_fields: None,
            max_inline_lines: None,
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedToolOutput {
    pub format: OutputFormat,
    pub schema_version: String,
    pub body: String,
    pub fallback_reason: Option<String>,
    pub truncation: Option<TruncationMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutputRenderError {
    UnsupportedFormat { format: OutputFormat },
    JsonSerialize { reason: String },
    OversizeOutput { reason: String },
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
            Self::OversizeOutput { reason } => {
                write!(f, "tool output exceeds byte budget: {reason}")
            }
        }
    }
}

impl Error for ToolOutputRenderError {}

pub fn render_tool_result(
    result: &ToolResult,
    options: &RenderOptions,
) -> Result<RenderedToolOutput, ToolOutputRenderError> {
    render_tool_result_with_policy(
        result,
        &ToolOutputRenderPolicy::from_render_options(options.clone()),
    )
}

pub fn render_tool_result_with_policy(
    result: &ToolResult,
    policy: &ToolOutputRenderPolicy,
) -> Result<RenderedToolOutput, ToolOutputRenderError> {
    let schema_version = schema_version(result);

    match policy.render_options.format {
        OutputFormat::Toon => {
            let (body, fallback_reason, truncation) = render_toon(result, policy, None)?;
            Ok(RenderedToolOutput {
                format: OutputFormat::Toon,
                schema_version,
                body,
                fallback_reason,
                truncation,
            })
        }
        OutputFormat::Text => {
            let (body, fallback_reason, truncation) = render_text(result, policy)?;
            Ok(RenderedToolOutput {
                format: OutputFormat::Text,
                schema_version,
                body,
                fallback_reason,
                truncation,
            })
        }
        OutputFormat::Json => {
            let (body, fallback_reason, truncation) = render_json(result, policy)?;

            Ok(RenderedToolOutput {
                format: OutputFormat::Json,
                schema_version,
                body,
                fallback_reason,
                truncation,
            })
        }
    }
}

fn render_toon(
    result: &ToolResult,
    policy: &ToolOutputRenderPolicy,
    fallback_reason: Option<&str>,
) -> Result<(String, Option<String>, Option<TruncationMetadata>), ToolOutputRenderError> {
    let (rendered_result, mut policy_fallback_reason) = renderable_result(result, policy)?;
    let truncation = rendered_result.truncation.clone();
    policy_fallback_reason = combine_fallback_reasons(
        policy_fallback_reason,
        render_policy_fallback_reason(result, &rendered_result),
    );
    let mut lines = Vec::new();

    lines.push(format!("status {}", status_name(rendered_result.status)));
    lines.push(format!(
        "schema_version {}",
        schema_version(&rendered_result)
    ));
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

    lines.push(format!(
        "tool_id {}",
        render_toon_value(&rendered_result.tool_id)
    ));
    lines.push(format!("result_id {}", rendered_result.id.as_str()));
    lines.push(format!(
        "invocation_id {}",
        rendered_result.invocation_id.as_str()
    ));

    if let Some(schema_ref) = &rendered_result.schema_ref {
        lines.push(format!("schema_ref {}", render_toon_value(schema_ref)));
    }

    let sections = toon_sections(&rendered_result);
    let mut truncation_reason = None;
    let mut body_lines = lines.len();

    if policy
        .max_inline_lines
        .is_some_and(|max_inline_lines| body_lines > max_inline_lines)
    {
        truncation_reason = Some(rendering_truncation_reason(
            &sections,
            policy.max_inline_lines,
            "tool output prelude",
            policy
                .max_inline_lines
                .map(|max_inline_lines| body_lines.saturating_sub(max_inline_lines)),
        ));
    }

    if truncation_reason.is_none() {
        for (index, section) in sections.iter().enumerate() {
            if section.lines.is_empty() {
                continue;
            }

            if policy
                .max_inline_lines
                .is_some_and(|max_inline_lines| body_lines + section.lines.len() > max_inline_lines)
            {
                truncation_reason = Some(rendering_truncation_reason(
                    &sections[index..],
                    policy.max_inline_lines,
                    section.label,
                    None,
                ));
                break;
            }

            body_lines += section.lines.len();
            lines.extend(section.lines.iter().cloned());
        }
    }

    if let Some(reason) = truncation_reason {
        let max_inline_lines = policy.max_inline_lines.unwrap_or(usize::MAX);
        let remaining_lines = max_inline_lines.saturating_sub(lines.len());
        let notice_lines = build_rendering_truncation_notice(&reason, remaining_lines);
        let notice_len = notice_lines.len();
        let keep_len = max_inline_lines.saturating_sub(notice_len);
        let mut kept_lines = lines.into_iter().take(keep_len).collect::<Vec<_>>();
        kept_lines.extend(notice_lines);
        let fallback_reason = combine_fallback_reasons(policy_fallback_reason, Some(reason));
        Ok((kept_lines.join("\n"), fallback_reason, truncation))
    } else {
        let fallback_reason = policy_fallback_reason;
        Ok((lines.join("\n"), fallback_reason, truncation))
    }
}

fn render_text(
    result: &ToolResult,
    policy: &ToolOutputRenderPolicy,
) -> Result<(String, Option<String>, Option<TruncationMetadata>), ToolOutputRenderError> {
    let (rendered_result, mut policy_fallback_reason) = renderable_result(result, policy)?;
    let truncation = rendered_result.truncation.clone();
    policy_fallback_reason = combine_fallback_reasons(
        policy_fallback_reason,
        render_policy_fallback_reason(result, &rendered_result),
    );
    let summary = summary_field(&rendered_result)
        .map(normalize_text_output_value)
        .unwrap_or_else(|| "no summary".to_string());
    let mut parts = vec![format!(
        "{} {}: {}",
        normalize_text_output_value(&rendered_result.tool_id),
        status_name(rendered_result.status),
        summary
    )];

    if !rendered_result.fields.is_empty() {
        parts.push(format!("{} field(s)", rendered_result.fields.len()));
    }

    if !rendered_result.evidence_refs.is_empty() {
        parts.push(format!(
            "{} evidence ref(s)",
            rendered_result.evidence_refs.len()
        ));
    }

    if !rendered_result.output_refs.is_empty() {
        parts.push(format!(
            "{} output ref(s)",
            rendered_result.output_refs.len()
        ));
    }

    if let Some(truncation) = &rendered_result.truncation {
        parts.push(format!(
            "truncated {}/{} bytes: {}",
            truncation.retained_bytes,
            truncation.original_bytes,
            normalize_text_output_value(&truncation.reason)
        ));
    }

    if !rendered_result.redactions.is_empty() {
        let markers = rendered_result
            .redactions
            .iter()
            .map(|redaction| {
                format!(
                    "{} ({}, redacted_at_ms {})",
                    normalize_text_output_value(&redaction.field_path),
                    normalize_text_output_value(&redaction.reason),
                    redaction.redacted_at.unix_millis
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        parts.push(format!(
            "{} redaction(s): {}",
            rendered_result.redactions.len(),
            markers
        ));
    }

    Ok((parts.join("; "), policy_fallback_reason, truncation))
}

fn render_json(
    result: &ToolResult,
    policy: &ToolOutputRenderPolicy,
) -> Result<(String, Option<String>, Option<TruncationMetadata>), ToolOutputRenderError> {
    let (rendered_result, mut policy_fallback_reason) = renderable_result(result, policy)?;
    let truncation = rendered_result.truncation.clone();
    policy_fallback_reason = combine_fallback_reasons(
        policy_fallback_reason,
        render_policy_fallback_reason(result, &rendered_result),
    );
    let pretty_body = serde_json::to_string_pretty(&rendered_result).map_err(|error| {
        ToolOutputRenderError::JsonSerialize {
            reason: error.to_string(),
        }
    })?;

    if policy
        .max_inline_lines
        .is_none_or(|max_inline_lines| pretty_body.lines().count() <= max_inline_lines)
    {
        return Ok((pretty_body, policy_fallback_reason, truncation));
    }

    let compact_fallback_reason = policy.max_inline_lines.map(|max_inline_lines| {
        format!("json rendering switched from pretty to compact to fit max_inline_lines={max_inline_lines}")
    });
    let mut result = rendered_result.clone();
    let compact_body =
        serde_json::to_string(&result).map_err(|error| ToolOutputRenderError::JsonSerialize {
            reason: error.to_string(),
        })?;

    if policy
        .max_inline_lines
        .is_none_or(|max_inline_lines| compact_body.lines().count() <= max_inline_lines)
    {
        return Ok((
            compact_body,
            combine_fallback_reasons(policy_fallback_reason, compact_fallback_reason),
            truncation,
        ));
    }

    let fallback_reason = policy
        .max_inline_lines
        .and_then(|max_inline_lines| truncate_json_rendering(&mut result, max_inline_lines));
    let body =
        serde_json::to_string(&result).map_err(|error| ToolOutputRenderError::JsonSerialize {
            reason: error.to_string(),
        })?;

    Ok((
        body,
        combine_fallback_reasons(
            combine_fallback_reasons(policy_fallback_reason, compact_fallback_reason),
            fallback_reason,
        ),
        truncation,
    ))
}

fn renderable_result(
    result: &ToolResult,
    policy: &ToolOutputRenderPolicy,
) -> Result<(ToolResult, Option<String>), ToolOutputRenderError> {
    let mut result = result.clone();
    result
        .fields
        .retain(|field| should_render_field(&field.key, &policy.render_options));
    if let Some(max_fields) = policy.max_fields {
        prioritize_renderable_fields(&mut result.fields);
        result.fields.truncate(max_fields);
    }
    if !policy.include_evidence_refs {
        result.evidence_refs.clear();
    }
    if !policy.include_output_refs {
        result.output_refs.clear();
    }
    if !policy.include_redactions {
        result.redactions.clear();
    }

    let mut fallback_reason = None;

    if let Some(max_inline_bytes) = policy.max_inline_bytes {
        let byte_budget_reason =
            apply_render_byte_budget(&mut result, max_inline_bytes, policy.oversize_policy)?;
        fallback_reason = combine_fallback_reasons(fallback_reason, byte_budget_reason);
    }

    Ok((result, fallback_reason))
}

fn apply_render_byte_budget(
    result: &mut ToolResult,
    max_inline_bytes: u64,
    oversize_policy: OversizeOutputPolicy,
) -> Result<Option<String>, ToolOutputRenderError> {
    let max_inline_bytes = max_inline_bytes_as_usize(max_inline_bytes);
    let oversized_fields = result
        .fields
        .iter()
        .filter_map(|field| {
            oversized_field_bytes(field, max_inline_bytes).map(|bytes| (field.key.clone(), bytes))
        })
        .collect::<Vec<_>>();

    if oversized_fields.is_empty() {
        return Ok(None);
    }

    let details = oversized_fields
        .iter()
        .map(|(key, size)| format!("{key} ({size} bytes)"))
        .collect::<Vec<_>>()
        .join(", ");

    match oversize_policy {
        OversizeOutputPolicy::RejectOversize => Err(ToolOutputRenderError::OversizeOutput {
            reason: format!(
                "tool output field(s) exceed max_inline_bytes={max_inline_bytes} with configured policy reject_oversize: {details}"
            ),
        }),
        OversizeOutputPolicy::TruncateWithMetadata | OversizeOutputPolicy::SpillToArtifactRef => {
            let mut omitted_fields = 0usize;
            let mut original_bytes = 0u64;
            let mut retained_bytes = 0u64;

            for field in &mut result.fields {
                let Some(bytes) = oversized_field_bytes(field, max_inline_bytes) else {
                    continue;
                };

                original_bytes = original_bytes.saturating_add(bytes as u64);
                omitted_fields += 1;

                match &field.value {
                    StructuredValue::String(value) => {
                        if let Some(truncated_value) = truncate_string_value(value, max_inline_bytes) {
                            retained_bytes = retained_bytes.saturating_add(truncated_value.len() as u64);
                            field.value = StructuredValue::String(truncated_value);
                        }
                    }
                    StructuredValue::StringList(values) => {
                        let (truncated_values, retained) = truncate_string_list(values, max_inline_bytes);
                        retained_bytes = retained_bytes.saturating_add(retained);
                        field.value = StructuredValue::StringList(truncated_values);
                    }
                    _ => {}
                }
            }

            if omitted_fields == 0 {
                return Ok(None);
            }

            let rendered_truncation = TruncationMetadata {
                original_bytes,
                retained_bytes,
                reason: format!(
                    "rendering truncated oversized tool output to max_inline_bytes={max_inline_bytes}"
                ),
            };

            result.truncation = Some(match result.truncation.take() {
                Some(existing) => TruncationMetadata {
                    original_bytes: existing.original_bytes.saturating_add(rendered_truncation.original_bytes),
                    retained_bytes: existing.retained_bytes.saturating_add(rendered_truncation.retained_bytes),
                    reason: format!(
                        "{}; {}",
                        existing.reason, rendered_truncation.reason
                    ),
                },
                None => rendered_truncation,
            });

            Ok(Some(format!(
                "rendering truncated oversized output to max_inline_bytes={max_inline_bytes}; omitted fields={omitted_fields}, details={details}"
            )))
        }
    }
}

fn max_inline_bytes_as_usize(max_inline_bytes: u64) -> usize {
    match usize::try_from(max_inline_bytes) {
        Ok(max_inline_bytes) => max_inline_bytes,
        Err(_) => usize::MAX,
    }
}

fn oversized_field_bytes(
    field: &crate::domain::ToolResultField,
    max_inline_bytes: usize,
) -> Option<usize> {
    let bytes = match &field.value {
        StructuredValue::String(value) => value.len(),
        StructuredValue::StringList(values) => {
            if values.is_empty() {
                0
            } else {
                values.iter().map(|value| value.len()).sum::<usize>() + (values.len() - 1)
            }
        }
        _ => return None,
    };

    if bytes > max_inline_bytes {
        Some(bytes)
    } else {
        None
    }
}

fn truncate_string_value(value: &str, max_inline_bytes: usize) -> Option<String> {
    if value.len() <= max_inline_bytes {
        return None;
    }

    let mut end = max_inline_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    Some(value[..end].to_string())
}

fn truncate_string_list(values: &[String], max_inline_bytes: usize) -> (Vec<String>, u64) {
    let mut kept_values = Vec::new();
    let mut kept_bytes = 0usize;

    for value in values {
        let separator = usize::from(!kept_values.is_empty());
        let remaining = max_inline_bytes.saturating_sub(kept_bytes.saturating_add(separator));
        if remaining == 0 {
            break;
        }

        let Some(truncated) = truncate_string_value(value, remaining).or_else(|| {
            if value.len() <= remaining {
                Some(value.clone())
            } else {
                None
            }
        }) else {
            break;
        };

        kept_bytes = kept_bytes.saturating_add(separator + truncated.len());
        kept_values.push(truncated);
    }

    (kept_values, kept_bytes as u64)
}

fn prioritize_renderable_fields(fields: &mut [crate::domain::ToolResultField]) {
    fields.sort_by_key(|field| field.key != "summary");
}

fn render_policy_fallback_reason(original: &ToolResult, rendered: &ToolResult) -> Option<String> {
    let omitted_fields = original.fields.len().saturating_sub(rendered.fields.len());
    let omitted_evidence_refs = original
        .evidence_refs
        .len()
        .saturating_sub(rendered.evidence_refs.len());
    let omitted_output_refs = original
        .output_refs
        .len()
        .saturating_sub(rendered.output_refs.len());
    let omitted_redactions = original
        .redactions
        .len()
        .saturating_sub(rendered.redactions.len());

    let compacted_by_policy = omitted_fields > 0
        || omitted_evidence_refs > 0
        || omitted_output_refs > 0
        || omitted_redactions > 0;

    let mut reasons = Vec::new();

    if compacted_by_policy {
        reasons.push(format!(
            "render policy compacted output; omitted fields={omitted_fields}, evidence_refs={omitted_evidence_refs}, output_refs={omitted_output_refs}, redactions={omitted_redactions}"
        ));
    }

    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

fn combine_fallback_reasons(
    policy_reason: Option<String>,
    truncation_reason: Option<String>,
) -> Option<String> {
    match (policy_reason, truncation_reason) {
        (None, None) => None,
        (Some(reason), None) | (None, Some(reason)) => Some(reason),
        (Some(policy_reason), Some(truncation_reason)) => {
            Some(format!("{policy_reason}; {truncation_reason}"))
        }
    }
}

fn truncate_json_rendering(result: &mut ToolResult, max_inline_lines: usize) -> Option<String> {
    if serde_json::to_string(result)
        .ok()
        .is_some_and(|body| body.lines().count() <= max_inline_lines)
    {
        return None;
    }

    let mut remaining = max_inline_lines;
    let mut omitted_fields = 0usize;

    if result.fields.len() > remaining {
        omitted_fields = result.fields.len() - remaining;
        result.fields.truncate(remaining);
    }

    remaining = remaining.saturating_sub(result.fields.len());

    let omitted_evidence_refs;
    let omitted_output_refs;
    let omitted_redactions;

    if remaining == 0 {
        omitted_evidence_refs = result.evidence_refs.len();
        omitted_output_refs = result.output_refs.len();
        omitted_redactions = result.redactions.len();
        result.evidence_refs.clear();
        result.output_refs.clear();
        result.redactions.clear();
    } else {
        let keep = remaining.min(result.evidence_refs.len());
        omitted_evidence_refs = result.evidence_refs.len() - keep;
        result.evidence_refs.truncate(keep);
        remaining -= keep;

        let keep = remaining.min(result.output_refs.len());
        omitted_output_refs = result.output_refs.len() - keep;
        result.output_refs.truncate(keep);
        remaining -= keep;

        let keep = remaining.min(result.redactions.len());
        omitted_redactions = result.redactions.len() - keep;
        result.redactions.truncate(keep);
    }

    if omitted_fields == 0
        && omitted_evidence_refs == 0
        && omitted_output_refs == 0
        && omitted_redactions == 0
    {
        None
    } else {
        let body_fits = serde_json::to_string(result)
            .ok()
            .is_some_and(|body| body.lines().count() <= max_inline_lines);

        if body_fits {
            None
        } else {
            Some(format!(
                "json rendering truncated to max_inline_lines={max_inline_lines}; omitted fields={omitted_fields}, evidence_refs={omitted_evidence_refs}, output_refs={omitted_output_refs}, redactions={omitted_redactions}"
            ))
        }
    }
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

#[derive(Debug, Clone)]
struct ToonSection {
    label: &'static str,
    count: usize,
    lines: Vec<String>,
}

fn toon_sections(result: &ToolResult) -> Vec<ToonSection> {
    let mut sections = Vec::new();
    sections.push(ToonSection {
        label: "fields",
        count: result.fields.len(),
        lines: build_fields_section(&result.fields),
    });
    sections.push(ToonSection {
        label: "evidence_refs",
        count: result.evidence_refs.len(),
        lines: build_artifact_refs_section("evidence_refs", &result.evidence_refs),
    });
    sections.push(ToonSection {
        label: "output_refs",
        count: result.output_refs.len(),
        lines: build_output_refs_section(&result.output_refs),
    });
    if let Some(truncation) = &result.truncation {
        sections.push(ToonSection {
            label: "truncation",
            count: 1,
            lines: build_truncation_section(truncation),
        });
    }
    sections.push(ToonSection {
        label: "redactions",
        count: result.redactions.len(),
        lines: build_redactions_section(&result.redactions),
    });

    sections
}

fn build_fields_section(fields: &[crate::domain::ToolResultField]) -> Vec<String> {
    if fields.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::with_capacity(fields.len() + 1);
    lines.push(format!("fields[{}]{{key,value}}", fields.len()));
    for field in fields {
        lines.push(format!(
            "  {},{}",
            render_toon_value(&field.key),
            render_structured_value(&field.value)
        ));
    }

    lines
}

fn build_artifact_refs_section(label: &str, refs: &[ArtifactRef]) -> Vec<String> {
    if refs.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::with_capacity(refs.len() + 1);
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

    lines
}

fn build_output_refs_section(refs: &[OutputRef]) -> Vec<String> {
    if refs.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::with_capacity(refs.len() + 1);
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

    lines
}

fn build_truncation_section(truncation: &TruncationMetadata) -> Vec<String> {
    vec![
        "truncation".to_string(),
        "  truncated true".to_string(),
        format!("  original_bytes {}", truncation.original_bytes),
        format!("  retained_bytes {}", truncation.retained_bytes),
        format!("  reason {}", render_toon_value(&truncation.reason)),
    ]
}

fn build_redactions_section(redactions: &[RedactionMarker]) -> Vec<String> {
    if redactions.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::with_capacity(redactions.len() + 1);
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

    lines
}

fn rendering_truncation_reason(
    omitted_sections: &[ToonSection],
    max_inline_lines: Option<usize>,
    omitted_section: &str,
    prelude_lines_omitted: Option<usize>,
) -> String {
    let omitted_counts = format_omitted_section_counts(omitted_sections);
    let max_inline_lines =
        max_inline_lines.map_or_else(|| "unbounded".to_string(), |value| value.to_string());
    let prelude_lines_omitted = prelude_lines_omitted
        .map(|value| format!("; prelude_lines_omitted={value}"))
        .unwrap_or_default();
    format!(
        "toon rendering truncated to max_inline_lines={max_inline_lines} before {omitted_section}{prelude_lines_omitted}; omitted {omitted_counts}"
    )
}

fn build_rendering_truncation_notice(reason: &str, remaining_lines: usize) -> Vec<String> {
    match remaining_lines {
        0 | 1 => vec![format!(
            "rendering_truncated true; rendering_truncation_reason {}",
            render_toon_value(reason)
        )],
        _ => vec![
            "rendering_truncated true".to_string(),
            format!("rendering_truncation_reason {}", render_toon_value(reason)),
        ],
    }
}

fn format_omitted_section_counts(sections: &[ToonSection]) -> String {
    let fields = sections
        .iter()
        .filter(|section| section.label == "fields")
        .map(|section| section.count)
        .sum::<usize>();
    let evidence_refs = sections
        .iter()
        .filter(|section| section.label == "evidence_refs")
        .map(|section| section.count)
        .sum::<usize>();
    let output_refs = sections
        .iter()
        .filter(|section| section.label == "output_refs")
        .map(|section| section.count)
        .sum::<usize>();
    let truncation = sections
        .iter()
        .filter(|section| section.label == "truncation")
        .map(|section| section.count)
        .sum::<usize>();
    let redactions = sections
        .iter()
        .filter(|section| section.label == "redactions")
        .map(|section| section.count)
        .sum::<usize>();

    format!(
        "fields={fields}, evidence_refs={evidence_refs}, output_refs={output_refs}, truncation={truncation}, redactions={redactions}"
    )
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

fn normalize_text_output_value(value: &str) -> String {
    if !value
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
    {
        return value.to_string();
    }

    let mut normalized = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                normalized.push(' ');
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
            }
            '\n' => normalized.push(' '),
            _ => normalized.push(character),
        }
    }

    normalized
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

    fn result_with_oversized_summary(summary: &str) -> ToolResult {
        let mut result = sample_result();
        result.fields[0].value = StructuredValue::String(summary.to_string());
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
        assert_eq!(rendered.fallback_reason, None);
        assert_eq!(
            rendered.body,
            "git_status succeeded: 2 modified files; 3 field(s); 1 evidence ref(s); 1 output ref(s)"
        );
    }

    #[test]
    fn defaults_render_policy_applies_byte_budget_to_runtime_rendering() {
        let defaults = crate::settings::ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Text),
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            ..crate::settings::ToolOutputDefaults::default()
        };
        let policy = defaults.render_policy();
        let result = result_with_oversized_summary("1234567890");

        assert_eq!(policy.max_inline_bytes, Some(8));
        assert_eq!(
            policy.oversize_policy,
            OversizeOutputPolicy::TruncateWithMetadata
        );

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert!(rendered.body.contains("git_status succeeded: 12345678"));
        assert!(rendered.body.contains("truncated "));
        assert!(rendered
            .body
            .contains("rendering truncated oversized tool output to max_inline_bytes=8"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("max_inline_bytes=8"));
    }

    #[test]
    fn defaults_render_policy_can_reject_oversized_runtime_rendering() {
        let defaults = crate::settings::ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Text),
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::RejectOversize,
            ..crate::settings::ToolOutputDefaults::default()
        };
        let policy = defaults.render_policy();
        let result = result_with_oversized_summary("1234567890");

        let error = render_tool_result_with_policy(&result, &policy).unwrap_err();

        assert!(matches!(
            error,
            ToolOutputRenderError::OversizeOutput { .. }
        ));
        assert!(error.to_string().contains("max_inline_bytes=8"));
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
    fn json_rendering_preserves_pretty_output_when_inline_budget_is_sufficient() {
        let result = sample_result();
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_fields: None,
            max_inline_lines: Some(64),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(rendered.fallback_reason, None);
        assert!(rendered.body.lines().count() > 1);
        assert!(rendered.body.starts_with("{\n"));
        assert!(rendered.body.contains("\n  \"tool_id\": \"git_status\""));
        assert!(rendered.body.contains("\n  \"fields\": ["));
    }

    #[test]
    fn json_rendering_sets_fallback_reason_when_render_policy_compacts_output() {
        let result = result_with_optional_channels();
        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Json)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(json["fields"].as_array().unwrap().len(), 1);
        assert_eq!(json["fields"][0]["key"], "summary");
        assert_eq!(
            json["fields"][0]["value"]["string"],
            "optional channel sample"
        );
        assert!(!rendered.body.contains("\"policy.state\""));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
    }

    #[test]
    fn json_rendering_sets_fallback_reason_when_pretty_output_exceeds_inline_budget_but_compact_fits(
    ) {
        let mut result = sample_result();
        result.fields.push(ToolResultField {
            key: "debug_note".to_string(),
            value: StructuredValue::String("keep me out".to_string()),
        });
        result.fields.push(ToolResultField {
            key: "extra_note".to_string(),
            value: StructuredValue::String("still keep me out".to_string()),
        });
        result.evidence_refs.push(ArtifactRef {
            id: ArtifactRefId::new(),
            uri: "/tmp/evidence-2.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("secondary evidence".to_string()),
            digest: Some("sha256:def456".to_string()),
        });
        result.output_refs.push(OutputRef {
            id: OutputRefId::new(),
            uri: "/tmp/stdout-2.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("stdout-2".to_string()),
            digest: Some("sha256:ghi789".to_string()),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.debug_note".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_fields: None,
            max_inline_lines: Some(2),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(rendered.body.lines().count(), 1);
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("json rendering switched from pretty to compact"));
        assert_eq!(
            json["fields"].as_array().unwrap().len(),
            5,
            "compact JSON should keep all renderable fields when it fits"
        );
        assert_eq!(json["evidence_refs"].as_array().unwrap().len(), 2);
        assert_eq!(json["output_refs"].as_array().unwrap().len(), 2);
        assert_eq!(json["redactions"].as_array().unwrap().len(), 1);
        assert_eq!(json["fields"][0]["key"], "summary");
        assert_eq!(json["fields"][1]["key"], "changed_files");
        assert!(rendered.body.contains("debug_note"));
        assert!(rendered.body.contains("secondary evidence"));
    }

    #[test]
    fn json_rendering_combines_compact_and_truncation_fallback_reasons() {
        let mut result = sample_result();
        result.fields.push(ToolResultField {
            key: "debug_note".to_string(),
            value: StructuredValue::String("keep me out".to_string()),
        });
        result.evidence_refs.push(ArtifactRef {
            id: ArtifactRefId::new(),
            uri: "/tmp/evidence-2.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("secondary evidence".to_string()),
            digest: Some("sha256:def456".to_string()),
        });
        result.output_refs.push(OutputRef {
            id: OutputRefId::new(),
            uri: "/tmp/stdout-2.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("stdout-2".to_string()),
            digest: Some("sha256:ghi789".to_string()),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.debug_note".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_fields: None,
            max_inline_lines: Some(0),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(rendered.body.lines().count(), 1);
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("json rendering switched from pretty to compact"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("json rendering truncated to max_inline_lines=0"));
        assert_eq!(json["fields"].as_array().unwrap().len(), 0);
        assert_eq!(json["evidence_refs"].as_array().unwrap().len(), 0);
        assert_eq!(json["output_refs"].as_array().unwrap().len(), 0);
        assert_eq!(json["redactions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn json_rendering_surfaces_render_time_truncation_metadata() {
        let mut result = sample_result();
        result.fields[1].value = StructuredValue::String("0123456789abcdef".to_string());
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::new(OutputFormat::Json),
            max_fields: None,
            max_inline_lines: None,
            max_inline_bytes: Some(8),
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let truncation = rendered
            .truncation
            .expect("rendering should include truncation metadata");

        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();
        assert_eq!(json["tool_id"], "git_status");
        assert_eq!(
            truncation.reason,
            "rendering truncated oversized tool output to max_inline_bytes=8"
        );
    }

    #[test]
    fn toon_rendering_emits_truncation_notice_when_budget_is_smaller_than_prelude() {
        let result = sample_result();
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: None,
            max_inline_lines: Some(1),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert_eq!(rendered.format, OutputFormat::Toon);
        assert_eq!(rendered.body.lines().count(), 1);
        assert!(rendered.body.contains("rendering_truncated true"));
        assert!(rendered.body.contains("rendering_truncation_reason"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("max_inline_lines=1"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("prelude_lines_omitted="));
    }

    #[test]
    fn toon_rendering_surfaces_render_time_truncation_metadata() {
        let mut result = sample_result();
        result.fields[1].value = StructuredValue::String("0123456789abcdef".to_string());
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: None,
            max_inline_lines: None,
            max_inline_bytes: Some(8),
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let truncation = rendered
            .truncation
            .expect("rendering should include truncation metadata");

        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("rendering truncated oversized output"));
        assert_eq!(
            truncation.reason,
            "rendering truncated oversized tool output to max_inline_bytes=8"
        );
    }

    #[test]
    fn toon_rendering_sets_fallback_reason_when_render_policy_compacts_output() {
        let result = result_with_optional_channels();
        let rendered = render_tool_result(&result, &RenderOptions::default()).unwrap();

        assert_eq!(rendered.format, OutputFormat::Toon);
        assert!(rendered.body.contains("fields[1]{key,value}"));
        assert!(!rendered.body.contains("policy.state"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
    }

    #[test]
    fn text_rendering_sets_fallback_reason_when_render_policy_compacts_output() {
        let result = result_with_optional_channels();
        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();

        assert_eq!(rendered.format, OutputFormat::Text);
        assert!(rendered
            .body
            .starts_with("git_status succeeded: optional channel sample"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
    }

    #[test]
    fn text_rendering_normalizes_dynamic_line_breaks_to_keep_single_line_output() {
        let mut result = sample_result();
        result.tool_id = "git_status\r\nsecondary".to_string();
        result.fields[0].value = StructuredValue::String("2 modified\r\nfiles".to_string());
        result.truncation = Some(TruncationMetadata {
            original_bytes: 4096,
            retained_bytes: 512,
            reason: "artifact\r\nthreshold".to_string(),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.secret\r\nnote".to_string(),
            reason: "policy\r\nsecret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let rendered =
            render_tool_result(&result, &RenderOptions::new(OutputFormat::Text)).unwrap();

        assert_eq!(rendered.body.lines().count(), 1);
        assert_eq!(
            rendered.body,
            "git_status secondary succeeded: 2 modified files; 3 field(s); 1 evidence ref(s); 1 output ref(s); truncated 512/4096 bytes: artifact threshold; 1 redaction(s): fields.secret note (policy secret, redacted_at_ms 1700000000123)"
        );
    }

    #[test]
    fn text_rendering_surfaces_render_time_truncation_metadata() {
        let mut result = sample_result();
        result.fields[1].value = StructuredValue::String("0123456789abcdef".to_string());
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::new(OutputFormat::Text),
            max_fields: None,
            max_inline_lines: None,
            max_inline_bytes: Some(8),
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let truncation = rendered
            .truncation
            .expect("rendering should include truncation metadata");

        assert!(rendered.body.contains("truncated"));
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("rendering truncated oversized output"));
        assert_eq!(
            truncation.reason,
            "rendering truncated oversized tool output to max_inline_bytes=8"
        );
    }

    #[test]
    fn render_policy_limits_fields_and_secondary_sections() {
        let mut result = sample_result();
        result.fields.push(ToolResultField {
            key: "debug_note".to_string(),
            value: StructuredValue::String("keep me out".to_string()),
        });
        result.redactions = vec![RedactionMarker {
            field_path: "fields.debug_note".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];

        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: Some(2),
            max_inline_lines: Some(16),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: false,
            include_output_refs: false,
            include_redactions: false,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert!(rendered.body.contains("fields[2]{key,value}"));
        assert!(rendered.body.contains("  summary,\"2 modified files\""));
        assert!(rendered.body.contains(
            "  changed_files,[crates/atelia-core/src/lib.rs,docs/tool-output-schema.md]"
        ));
        assert!(!rendered.body.contains("debug_note"));
        assert!(!rendered.body.contains("evidence_refs["));
        assert!(!rendered.body.contains("output_refs["));
        assert!(!rendered.body.contains("redactions["));
    }

    #[test]
    fn render_policy_max_fields_prioritizes_summary_before_truncation() {
        let mut result = sample_result();
        result.fields = vec![
            ToolResultField {
                key: "policy.state".to_string(),
                value: StructuredValue::String("allowed_with_audit".to_string()),
            },
            ToolResultField {
                key: "changed_files".to_string(),
                value: StructuredValue::StringList(vec![
                    "crates/atelia-core/src/lib.rs".to_string(),
                    "docs/tool-output-schema.md".to_string(),
                ]),
            },
            ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("summary survives truncation".to_string()),
            },
            ToolResultField {
                key: "diagnostics.parser_failure".to_string(),
                value: StructuredValue::String("none".to_string()),
            },
        ];

        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: true,
                include_diagnostics: true,
                include_cost: false,
            },
            max_fields: Some(2),
            max_inline_lines: None,
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: false,
            include_output_refs: false,
            include_redactions: false,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();
        let keys = json["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["key"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(keys, vec!["summary", "policy.state"]);
        assert_eq!(
            json["fields"][0]["value"]["string"],
            "summary survives truncation"
        );
        assert!(
            rendered.body.find("\"key\": \"summary\"").unwrap()
                < rendered.body.find("\"key\": \"policy.state\"").unwrap()
        );
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
    fn toon_rendering_truncates_before_secondary_sections_with_marker() {
        let mut result = sample_result();
        result.redactions = vec![RedactionMarker {
            field_path: "fields.exit_code".to_string(),
            reason: "policy secret".to_string(),
            redacted_at: LedgerTimestamp::from_unix_millis(1_700_000_000_123),
        }];
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: Some(3),
            max_inline_lines: Some(15),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();
        let lines = rendered.body.lines().collect::<Vec<_>>();

        assert!(lines.len() <= 15);
        assert!(rendered.body.contains("fields[3]{key,value}"));
        assert!(rendered
            .body
            .contains("evidence_refs[1]{id,uri,media_type,label,digest}"));
        assert!(!rendered.body.contains("redactions["));
        assert!(rendered.body.contains("rendering_truncated true"));
        assert!(rendered.body.contains("rendering_truncation_reason"));
    }

    #[test]
    fn toon_rendering_reports_truncation_when_fields_do_not_fit() {
        let result = sample_result();
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: Some(3),
            max_inline_lines: Some(9),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: true,
            include_output_refs: true,
            include_redactions: true,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert_eq!(rendered.format, OutputFormat::Toon);
        assert!(rendered.body.lines().count() <= 9);
        assert!(rendered.body.contains("rendering_truncated true"));
        assert!(rendered.body.contains("rendering_truncation_reason"));
        assert!(!rendered.body.contains("fields["));
        assert!(rendered
            .body
            .contains("schema_ref \"schema:git.status.v1\""));
    }

    #[test]
    fn toon_rendering_uses_a_single_line_notice_when_only_one_line_remains() {
        let result = ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::from_unix_millis(1_700_000_000_000),
            invocation_id: ToolInvocationId::new(),
            tool_id: "git_status".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: None,
            fields: vec![ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("one field".to_string()),
            }],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        };
        let policy = ToolOutputRenderPolicy {
            render_options: RenderOptions::default(),
            max_fields: Some(1),
            max_inline_lines: Some(7),
            max_inline_bytes: None,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            include_evidence_refs: false,
            include_output_refs: false,
            include_redactions: false,
        };

        let rendered = render_tool_result_with_policy(&result, &policy).unwrap();

        assert!(rendered.body.lines().count() <= 7);
        assert!(rendered.body.contains("rendering_truncation_reason"));
        assert!(!rendered.body.contains("rendering_truncated true\n"));
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
