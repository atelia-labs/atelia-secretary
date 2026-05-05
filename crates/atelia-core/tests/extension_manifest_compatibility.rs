use atelia_core::extensions::{ExtensionBoundary, ExtensionManifest, ManifestValidationPolicy};

/// Deserialize a manifest fixture into the extension manifest model.
fn load_manifest_fixture(contents: &str) -> ExtensionManifest {
    serde_json::from_str(contents).expect("fixture manifest should deserialize")
}

/// Assert that a fixture round-trips through serialization and validation.
fn assert_manifest_fixture_roundtrip(
    contents: &str,
    policy: ManifestValidationPolicy,
    boundary: ExtensionBoundary,
) {
    let manifest = load_manifest_fixture(contents);
    let serialized = serde_json::to_value(&manifest).expect("fixture manifest should serialize");
    let fixture = serde_json::from_str::<serde_json::Value>(contents)
        .expect("fixture manifest should parse as JSON");

    assert_eq!(
        serialized, fixture,
        "fixture should round-trip through the manifest model"
    );

    let validated = manifest
        .validate(&policy)
        .expect("fixture manifest should validate");
    assert_eq!(validated.boundary, boundary);
}

/// Verify compatibility coverage for third-party backend manifest fixtures.
#[test]
fn extension_manifest_compatibility_fixtures_cover_third_party_backend_manifests() {
    assert_manifest_fixture_roundtrip(
        include_str!("fixtures/extensions/third_party_backend.json"),
        ManifestValidationPolicy::default(),
        ExtensionBoundary::ThirdParty,
    );
}

/// Verify compatibility coverage for local process manifest fixtures.
#[test]
fn extension_manifest_compatibility_fixtures_cover_local_process_manifests() {
    assert_manifest_fixture_roundtrip(
        include_str!("fixtures/extensions/local_process.json"),
        ManifestValidationPolicy::default()
            .with_local_unsigned()
            .with_local_process_runtime(),
        ExtensionBoundary::LocalDevelopment,
    );
}
