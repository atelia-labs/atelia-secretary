use atelia_core::domain::{
    ArtifactRef, ArtifactRefId, LedgerTimestamp, OutputRef, OutputRefId, StructuredValue,
    ToolInvocationId, ToolResult, ToolResultField, ToolResultId, ToolResultStatus,
};
use atelia_core::tool_output::{
    render_tool_result, render_tool_result_with_policy, OutputFormat, RenderOptions,
    ToolOutputRenderPolicy,
};
use atelia_core::OversizeOutputPolicy;

/// Builds the canonical tool-result fixture used by the default render tests.
fn fixed_tool_result() -> ToolResult {
    ToolResult {
        id: ToolResultId::try_from_string("res_00000000-0000-4000-8000-000000000001")
            .expect("valid tool result id"),
        schema_version: 1,
        created_at: LedgerTimestamp::from_unix_millis(1_700_000_000_000),
        invocation_id: ToolInvocationId::try_from_string(
            "tool_00000000-0000-4000-8000-000000000002",
        )
        .expect("valid tool invocation id"),
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
            id: ArtifactRefId::try_from_string("art_00000000-0000-4000-8000-000000000003")
                .expect("valid artifact ref id"),
            uri: "/tmp/evidence.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("status evidence".to_string()),
            digest: Some("sha256:abc123".to_string()),
        }],
        output_refs: vec![OutputRef {
            id: OutputRefId::try_from_string("out_00000000-0000-4000-8000-000000000004")
                .expect("valid output ref id"),
            uri: "/tmp/stdout.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("stdout".to_string()),
            digest: None,
        }],
        truncation: None,
        redactions: Vec::new(),
    }
}

/// Builds the fixture used to exercise customizer-style field filtering.
fn customizer_fixture_result() -> ToolResult {
    ToolResult {
        id: ToolResultId::try_from_string("res_00000000-0000-4000-8000-000000000005")
            .expect("valid tool result id"),
        schema_version: 1,
        created_at: LedgerTimestamp::from_unix_millis(1_700_000_123_000),
        invocation_id: ToolInvocationId::try_from_string(
            "tool_00000000-0000-4000-8000-000000000006",
        )
        .expect("valid tool invocation id"),
        tool_id: "customizer".to_string(),
        status: ToolResultStatus::Succeeded,
        schema_ref: Some("schema:tool.customizer.v1".to_string()),
        fields: vec![
            ToolResultField {
                key: "policy.state".to_string(),
                value: StructuredValue::String("allowed_with_audit".to_string()),
            },
            ToolResultField {
                key: "diagnostics.parser_failure".to_string(),
                value: StructuredValue::String("none".to_string()),
            },
            ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("customizer keeps summary".to_string()),
            },
            ToolResultField {
                key: "cost.output_tokens".to_string(),
                value: StructuredValue::Integer(128),
            },
        ],
        evidence_refs: Vec::new(),
        output_refs: Vec::new(),
        truncation: None,
        redactions: Vec::new(),
    }
}

/// Builds a fixture that keeps evidence/output refs while still compacting fields.
fn customizer_refs_fixture_result() -> ToolResult {
    ToolResult {
        id: ToolResultId::try_from_string("res_00000000-0000-4000-8000-000000000007")
            .expect("valid tool result id"),
        schema_version: 1,
        created_at: LedgerTimestamp::from_unix_millis(1_700_000_246_000),
        invocation_id: ToolInvocationId::try_from_string(
            "tool_00000000-0000-4000-8000-000000000008",
        )
        .expect("valid tool invocation id"),
        tool_id: "customizer".to_string(),
        status: ToolResultStatus::Succeeded,
        schema_ref: Some("schema:tool.customizer.v1".to_string()),
        fields: vec![
            ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("customizer keeps refs".to_string()),
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
                value: StructuredValue::Integer(256),
            },
        ],
        evidence_refs: vec![ArtifactRef {
            id: ArtifactRefId::try_from_string("art_00000000-0000-4000-8000-000000000009")
                .expect("valid artifact ref id"),
            uri: "/tmp/customizer-evidence.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("customizer evidence".to_string()),
            digest: Some("sha256:feedbeef".to_string()),
        }],
        output_refs: vec![OutputRef {
            id: OutputRefId::try_from_string("out_00000000-0000-4000-8000-000000000010")
                .expect("valid output ref id"),
            uri: "/tmp/customizer-stdout.txt".to_string(),
            media_type: "text/plain".to_string(),
            label: Some("customizer stdout".to_string()),
            digest: Some("sha256:c0ffee00".to_string()),
        }],
        truncation: None,
        redactions: Vec::new(),
    }
}

/// Returns the render policy used for the customizer fixture assertions.
fn customizer_fixture_policy(format: OutputFormat) -> ToolOutputRenderPolicy {
    ToolOutputRenderPolicy {
        render_options: RenderOptions {
            format,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        },
        max_fields: Some(2),
        max_inline_lines: None,
        max_inline_bytes: None,
        oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        include_evidence_refs: false,
        include_output_refs: false,
        include_redactions: false,
    }
}

/// Returns the render policy used for the customizer-with-refs fixture assertions.
fn customizer_refs_fixture_policy(format: OutputFormat) -> ToolOutputRenderPolicy {
    ToolOutputRenderPolicy {
        render_options: RenderOptions {
            format,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        },
        max_fields: Some(3),
        max_inline_lines: None,
        max_inline_bytes: None,
        oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        include_evidence_refs: true,
        include_output_refs: true,
        include_redactions: false,
    }
}

/// Verifies the canonical fixture renders identically for Toon and JSON defaults.
#[test]
fn tool_output_compatibility_fixtures_cover_toon_and_json_defaults() {
    let result = fixed_tool_result();

    let toon = render_tool_result(&result, &RenderOptions::default()).unwrap();
    let json = render_tool_result(&result, &RenderOptions::new(OutputFormat::Json)).unwrap();

    assert_eq!(toon.schema_version, "tool_result.v1");
    assert_eq!(toon.format, OutputFormat::Toon);
    assert_eq!(toon.fallback_reason, None);
    assert_eq!(
        toon.body.trim_end(),
        include_str!("fixtures/tool_output/canonical.toon").trim_end()
    );

    assert_eq!(json.schema_version, "tool_result.v1");
    assert_eq!(json.format, OutputFormat::Json);
    assert_eq!(json.fallback_reason, None);
    assert_eq!(
        json.body.trim_end(),
        include_str!("fixtures/tool_output/canonical.json").trim_end()
    );
}

/// Verifies customizer-style field filtering stays stable across formats.
#[test]
fn tool_output_compatibility_fixtures_cover_customizer_style_field_filtering() {
    let result = customizer_fixture_result();
    let policy = customizer_fixture_policy(OutputFormat::Toon);

    let toon = render_tool_result_with_policy(&result, &policy).unwrap();
    let json =
        render_tool_result_with_policy(&result, &customizer_fixture_policy(OutputFormat::Json))
            .unwrap();

    assert_eq!(toon.schema_version, "tool_result.v1");
    assert_eq!(toon.format, OutputFormat::Toon);
    assert!(toon
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("render policy compacted output"));
    assert_eq!(
        toon.body.trim_end(),
        include_str!("fixtures/tool_output/customizer_filtered.toon").trim_end()
    );

    assert_eq!(json.schema_version, "tool_result.v1");
    assert_eq!(json.format, OutputFormat::Json);
    assert!(json
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("render policy compacted output"));
    assert_eq!(
        json.body.trim_end(),
        include_str!("fixtures/tool_output/customizer_filtered.json").trim_end()
    );
}

/// Verifies customizer-style filtering stays stable when refs are retained.
#[test]
fn tool_output_compatibility_fixtures_cover_customizer_style_field_filtering_with_refs() {
    let result = customizer_refs_fixture_result();
    let policy = customizer_refs_fixture_policy(OutputFormat::Toon);

    let toon = render_tool_result_with_policy(&result, &policy).unwrap();
    let json = render_tool_result_with_policy(
        &result,
        &customizer_refs_fixture_policy(OutputFormat::Json),
    )
    .unwrap();

    assert_eq!(toon.schema_version, "tool_result.v1");
    assert_eq!(toon.format, OutputFormat::Toon);
    assert_eq!(toon.truncation, None);
    assert!(toon
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("render policy compacted output"));
    assert_eq!(
        toon.body.trim_end(),
        include_str!("fixtures/tool_output/customizer_with_refs.toon").trim_end()
    );

    assert_eq!(json.schema_version, "tool_result.v1");
    assert_eq!(json.format, OutputFormat::Json);
    assert_eq!(json.truncation, None);
    assert!(json
        .fallback_reason
        .as_deref()
        .unwrap()
        .contains("render policy compacted output"));
    assert_eq!(
        json.body.trim_end(),
        include_str!("fixtures/tool_output/customizer_with_refs.json").trim_end()
    );
}
