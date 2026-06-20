//! Integration test: feed the Rust engine_loader a copy of every real
//! `typio-engine-*.toml` shipped by the sibling engine repos, and verify
//! each parses, validates, and registers successfully into libtypio's
//! native `EngineRegistry`.
//!
//! This is the smoke test that catches drift between the manifest format
//! engines actually ship and the Rust port's understanding of it. If an
//! engine starts shipping a manifest that the Rust loader rejects (new
//! field, different value type, etc.), this test fails before the C
//! daemon's loader is retired.

use std::fs;
use std::path::PathBuf;

use typio_host::engine_loader::{EngineLoader, LoadError, SkipReason};

/// Real manifests shipped by sibling engine repos in this workspace.
/// Each entry is `(engine_dir, manifest_filename)`.
const REAL_MANIFESTS: &[(&str, &str)] = &[
    ("typio-engine-mozc", "typio-engine-mozc.toml"),
    ("typio-engine-rime", "typio-engine-rime.toml"),
    ("typio-engine-sherpa", "typio-engine-sherpa.toml"),
    ("typio-engine-whisper", "typio-engine-whisper.toml"),
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../typio-linux/crates/typio-host
    // workspace sibling checkouts (typio-engine-*, libtypio) live three
    // levels up: typio-host → crates → typio-linux → typio/ workspace.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join("../../..")
        .canonicalize()
        .expect("workspace root should canonicalise")
}

#[test]
fn all_real_manifests_parse_and_validate() {
    let mut checked = 0;
    for (engine_dir, manifest_name) in REAL_MANIFESTS {
        let path = workspace_root().join(engine_dir).join("build").join(manifest_name);
        if !path.exists() {
            eprintln!(
                "skipping {engine_dir}: build artifact {} not present",
                path.display()
            );
            continue;
        }
        let m = typio_host::engine_loader::manifest::EngineManifest::read_from(&path)
            .unwrap_or_else(|e| panic!("failed to parse real manifest {}: {e}", path.display()));
        assert!(
            m.has_required_fields(),
            "{manifest_name} from {engine_dir} is missing required fields"
        );
        assert_eq!(
            m.protocol, "typio-engine-protocol",
            "{manifest_name} from {engine_dir} declares unexpected protocol"
        );
        assert!(
            matches!(m.engine_type.as_str(), "keyboard" | "voice"),
            "{manifest_name} from {engine_dir} declares invalid type {:?}",
            m.engine_type
        );
        assert!(
            !m.effective_languages().is_empty(),
            "{manifest_name} from {engine_dir} declares no languages"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no engine build artifacts found — build at least one engine \
         (e.g. `cargo build --release` in typio-engine-compose/) before \
         running this test"
    );
}

#[test]
fn all_real_manifests_register_into_libtypio() {
    let mut loader = EngineLoader::new();
    // All engine repos ship `voice` type only for whisper; voice caps are
    // required for that one. Add them so the manifest isn't refused.
    loader.set_capabilities(
        typio_host::engine_loader::Capabilities::default().with_voice(),
    );

    let mut registry = typio::core::registry::EngineRegistry::new();

    for (engine_dir, manifest_name) in REAL_MANIFESTS {
        let path = workspace_root().join(engine_dir).join("build").join(manifest_name);
        if !path.exists() {
            eprintln!("skipping {engine_dir}: build artifact {} not present", path.display());
            continue;
        }
        loader
            .load_single(&mut registry, &path)
            .unwrap_or_else(|e| panic!("{engine_dir}: load_single failed: {e}"));
    }

    let keyboards = registry.list_keyboards();
    let voices = registry.list_voices();
    assert!(
        !keyboards.is_empty() || !voices.is_empty(),
        "expected at least one engine to register; keyboards={keyboards:?} voices={voices:?}"
    );
}

#[test]
fn duplicate_registration_of_same_engine_is_skipped_not_failed() {
    let manifest = REAL_MANIFESTS
        .iter()
        .find_map(|(dir, name)| {
            let p = workspace_root().join(dir).join("build").join(name);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .expect("at least one engine build artifact must exist for this test");

    let mut loader = EngineLoader::with_voice();
    let mut registry = typio::core::registry::EngineRegistry::new();
    loader
        .load_single(&mut registry, &manifest)
        .expect("first load");

    match loader.load_single(&mut registry, &manifest) {
        Err(LoadError::Skipped(SkipReason::AlreadyRegistered(_))) => (),
        other => panic!("expected AlreadyRegistered skip, got {other:?}"),
    }
}

/// A self-contained fixture: write a manifest into a temp dir at test time
/// (does not depend on sibling repos being built) and verify load_dir
/// produces the right LoadDirReport.
#[test]
fn fixture_load_dir_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();

    fs::write(
        dir.join("typio-engine-fixture.toml"),
        r#"
name = "fixture"
type = "keyboard"
protocol = "typio-engine-protocol"
display_name = "Fixture"
description = "test fixture"
command = "/nonexistent/typio-engine-fixture"
args = []
required = ["preedit"]
languages = ["und"]
"#,
    )
    .unwrap();

    // Create the icons dir BEFORE writing into it.
    fs::create_dir_all(dir.join("icons")).unwrap();

    let mut loader = EngineLoader::new();
    let mut registry = typio::core::registry::EngineRegistry::new();
    let report = loader.load_dir(&mut registry, dir);

    assert_eq!(report.registered, 1);
    assert!(report.skipped.is_empty());
    assert!(report.failed.is_empty());
    assert_eq!(registry.list_keyboards(), vec!["fixture"]);
    assert_eq!(
        loader.discovered_icon_theme_path(),
        Some(dir.join("icons").as_path())
    );
}
