//! Engine loader — discovers `typio-engine-*.toml` manifests on disk and
//! registers the engines they describe with libtypio's [`EngineRegistry`].
//!
//! Phase 1 port of `src/engine_loader.c` (678 lines of C). Replaces the
//! hand-rolled TOML parser, capability-set lookup, and path-resolution
//! helpers with idiomatic Rust on top of libtypio's native Rust API.
//!
//! ## What this module does NOT yet port
//!
//! - **Disabled-engine check** (`typio_is_engine_disabled` in C). The C
//!   version queries `keyboard.disabled` / `voice.disabled` from the
//!   TypioInstance config; the Rust host has not yet integrated Config,
//!   so this check is deferred. The wiring point is in
//!   [`EngineLoader::load_single`] — once Config lands, add a callback
//!   or a `&Config` reference and short-circuit before registration.
//! - **Reload/unload** (`typio_engine_loader_reload`,
//!   `typio_engine_loader_unload`). These are thin wrappers around
//!   [`EngineRegistry::unregister`] + reload; trivial to add when the
//!   daemon port reaches the IPC layer that drives them.
//!
//! ## Architecture
//!
//! The C version is a single 678-line file with global state (the
//! discovered icon theme path). The Rust port splits responsibilities:
//!
//! - [`manifest`] — TOML parsing + path resolution
//! - [`caps`] — host capability set + negotiation
//! - [`dirs`] — engine directory search-path resolution (ADR-0025)
//! - [`EngineLoader`] (this module) — orchestrates loading + registration

pub mod caps;
pub mod dirs;
pub mod manifest;

use std::path::{Path, PathBuf};

use typio::core::engine::backend::{process::ProcessBackend, EngineBackend};
use typio::core::engine::{BackendPreference, EngineCapabilities, EngineInfo, EngineType};
use typio::core::registry::EngineRegistry;

use caps::HostCapabilities;
use manifest::{is_manifest_filename, EngineManifest, ManifestError};

pub use caps::{HostCapabilities as Capabilities, NegotiationFailure};
pub use dirs::{find_manifest_for, resolve_engine_dirs, ENV_ENGINE_PATH, SYSTEM_ENGINE_DIR};
pub use manifest::{resolve_path_arg, ManifestError as Error, DEFAULT_LANGUAGE};

/// Loader state: a host capability set + a remembered icon theme path.
///
/// Equivalent to the file-scope globals (`typio_discovered_icon_theme_path`
/// and the static `TYPIOD_HOST_CAPABILITIES` array) in the C version, but
/// instance-scoped so multiple loaders could coexist (useful for tests).
#[derive(Debug, Clone)]
pub struct EngineLoader {
    caps: HostCapabilities,
    discovered_icon_theme_path: Option<PathBuf>,
}

impl Default for EngineLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineLoader {
    /// Construct a loader with the standard keyboard-IM host capabilities
    /// (no voice).
    pub fn new() -> Self {
        Self {
            caps: HostCapabilities::default(),
            discovered_icon_theme_path: None,
        }
    }

    /// Construct a loader with the keyboard-IM caps plus voice caps
    /// (`voice_input`, `continuous_voice`).
    pub fn with_voice() -> Self {
        Self {
            caps: HostCapabilities::default().with_voice(),
            discovered_icon_theme_path: None,
        }
    }

    /// Construct a loader with an explicit capability set.
    pub fn with_capabilities(caps: HostCapabilities) -> Self {
        Self {
            caps,
            discovered_icon_theme_path: None,
        }
    }

    /// Replace the host capability set. Affects subsequent `load_*` calls.
    pub fn set_capabilities(&mut self, caps: HostCapabilities) {
        self.caps = caps;
    }

    /// The first `<dir>/icons/` directory discovered during a `load_dir`
    /// call (or `None` if no scanned engine directory bundled an icons
    /// subdir). The C version surfaces this via
    /// `typio_engine_loader_discovered_icon_theme_path`.
    pub fn discovered_icon_theme_path(&self) -> Option<&Path> {
        self.discovered_icon_theme_path.as_deref()
    }

    /// Scan one directory and register every `typio-engine-*.toml` manifest
    /// found there into `registry`. Returns the count of successfully
    /// registered engines.
    ///
    /// After the scan, if the directory contains an `icons/` subdirectory
    /// and no icon path has been remembered yet, it is recorded and
    /// reachable via [`Self::discovered_icon_theme_path`].
    ///
    /// Errors opening the directory are silently downgraded to "0 engines
    /// registered" (matching the C version's `typio_log_debug` + return 0
    /// behaviour); per-manifest errors are returned individually via
    /// [`LoadOutcome`] in the returned log but do not abort the scan.
    pub fn load_dir(&mut self, registry: &mut EngineRegistry, dir: &Path) -> LoadDirReport {
        let mut report = LoadDirReport::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                // Match C: silent downgrade; caller treats 0 as "no engines here".
                return report;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(filename) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !is_manifest_filename(filename) {
                continue;
            }
            match self.load_single(registry, &path) {
                Ok(()) => report.registered += 1,
                Err(LoadError::Skipped(reason)) => report.skipped.push((path, reason)),
                Err(other) => report.failed.push((path, other)),
            }
        }

        // Icon theme discovery: first engine dir with icons/ wins.
        if self.discovered_icon_theme_path.is_none() {
            let icon_path = dir.join("icons");
            if icon_path.is_dir() {
                self.discovered_icon_theme_path = Some(icon_path);
            }
        }

        report
    }

    /// Load and register a single manifest by absolute path.
    ///
    /// Steps (mirror `typio_register_one` in C):
    ///   1. Parse the manifest.
    ///   2. Validate required fields (`name`, `type`, `protocol`, `command`).
    ///   3. Validate `protocol == "typio-engine-protocol"`.
    ///   4. Map `type` to [`EngineType`].
    ///   5. Negotiate capabilities (required must be a subset of host's caps).
    ///   6. Construct argv with path resolution.
    ///   7. Build a [`ProcessBackend`] and register it via libtypio's native
    ///      Rust API. Engine is `EngineError::AlreadyExists` if another
    ///      engine with the same name is already registered — surfaced as
    ///      [`LoadError::Skipped`] with reason `AlreadyRegistered` (the
    ///      registry registers the first of each name and skips later
    ///      duplicates, per ADR-0025).
    ///   8. If `languages` is present and non-empty, propagate them via
    ///      `set_engine_languages` (ADR-0031).
    pub fn load_single(
        &mut self,
        registry: &mut EngineRegistry,
        path: &Path,
    ) -> Result<(), LoadError> {
        let manifest = EngineManifest::read_from(path).map_err(LoadError::from)?;

        if !manifest.has_required_fields() {
            return Err(LoadError::Skipped(SkipReason::MissingRequiredFields));
        }
        if manifest.protocol != "typio-engine-protocol" {
            return Err(LoadError::Skipped(SkipReason::UnsupportedProtocol(
                manifest.protocol,
            )));
        }
        let engine_type = match manifest.engine_type.as_str() {
            "keyboard" => EngineType::Keyboard,
            "voice" => EngineType::Voice,
            other => {
                return Err(LoadError::Skipped(SkipReason::InvalidEngineType(
                    other.to_string(),
                )));
            }
        };

        // Capability negotiation.
        let required = manifest.required.clone().unwrap_or_default();
        let optional = manifest.optional.clone().unwrap_or_default();
        if let Err(failure) = self.caps.negotiate(&required, &optional) {
            return Err(LoadError::Skipped(SkipReason::MissingRequiredCapabilities(
                failure.missing_required,
            )));
        }

        // Build EngineInfo.
        let info = self.build_engine_info(&manifest, engine_type);

        // Build argv.
        let argv = manifest.argv(path)?;

        // Register via libtypio's native Rust API.
        let backend = EngineBackend::Process(ProcessBackend::new(info.clone(), argv));
        if let Err(err) = registry.register(backend) {
            match err {
                typio::core::engine::EngineError::AlreadyExists => {
                    return Err(LoadError::Skipped(SkipReason::AlreadyRegistered(
                        manifest.name,
                    )));
                }
                other => return Err(LoadError::RegisterFailed(other)),
            }
        }

        // Propagate declared languages to the registry (ADR-0031).
        if let Some(langs) = manifest.languages.as_ref() {
            if !langs.is_empty() {
                // set_engine_languages is a no-op (logs a warning) if the
                // engine is not registered — that should never happen here
                // because we just successfully registered.
                if let Err(err) = registry.set_engine_languages(&manifest.name, langs.clone()) {
                    return Err(LoadError::SetLanguagesFailed(err));
                }
            }
        }

        Ok(())
    }

    fn build_engine_info(&self, manifest: &EngineManifest, engine_type: EngineType) -> EngineInfo {
        EngineInfo {
            name: manifest.name.clone(),
            display_name: manifest
                .display_name
                .clone()
                .unwrap_or_else(|| manifest.name.clone()),
            description: manifest.description.clone().unwrap_or_default(),
            author: manifest.author.clone().unwrap_or_default(),
            icon: manifest.icon.clone(),
            language: manifest.primary_language(),
            languages: manifest.effective_languages(),
            engine_type,
            capabilities: EngineCapabilities {
                required: manifest.required.clone().unwrap_or_default(),
                optional: manifest.optional.clone().unwrap_or_default(),
            },
            backend_preference: BackendPreference::FfiPreferred,
        }
    }
}

/// Why a load call did not result in a registration, even though the manifest
/// was successfully located and parsed.
///
/// Skips are non-fatal: the C version logs at info/debug level and continues
/// the scan; the Rust version surfaces them in [`LoadDirReport::skipped`]
/// for the caller to decide how to surface.
#[derive(Debug, Clone)]
pub enum SkipReason {
    /// The manifest is missing one of `name`, `type`, `protocol`, `command`.
    MissingRequiredFields,
    /// `protocol` is set to something other than `typio-engine-protocol`.
    UnsupportedProtocol(String),
    /// `type` is not `keyboard` or `voice`.
    InvalidEngineType(String),
    /// Required capabilities are not a subset of the host's caps.
    MissingRequiredCapabilities(Vec<String>),
    /// Engine with this name already registered (ADR-0025: first wins).
    AlreadyRegistered(String),
}

/// Errors that prevent a manifest from loading. Distinguished from
/// [`SkipReason`] in that these indicate something is wrong with the file,
/// the system, or libtypio — not a deliberate skip.
#[derive(Debug)]
pub enum LoadError {
    /// Could not read, parse, or build argv from the manifest.
    Manifest(ManifestError),
    /// `EngineRegistry::register` failed with an error other than
    /// `AlreadyExists` (which is surfaced as [`LoadError::Skipped`]).
    RegisterFailed(typio::core::engine::EngineError),
    /// `EngineRegistry::set_engine_languages` failed after registration.
    SetLanguagesFailed(typio::core::engine::EngineError),
    /// The manifest was deliberately skipped — see [`SkipReason`].
    Skipped(SkipReason),
}

impl From<ManifestError> for LoadError {
    fn from(e: ManifestError) -> Self {
        LoadError::Manifest(e)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Manifest(e) => write!(f, "{e}"),
            LoadError::RegisterFailed(e) => write!(f, "registry register failed: {e}"),
            LoadError::SetLanguagesFailed(e) => {
                write!(f, "registry set_engine_languages failed: {e}")
            }
            LoadError::Skipped(reason) => write!(f, "skipped: {reason:?}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Per-directory summary returned by [`EngineLoader::load_dir`].
#[derive(Debug, Default)]
pub struct LoadDirReport {
    /// Count of engines successfully registered.
    pub registered: usize,
    /// Manifests located but deliberately skipped, with reason.
    pub skipped: Vec<(PathBuf, SkipReason)>,
    /// Manifests that failed to load (parse error, registry error, etc.).
    pub failed: Vec<(PathBuf, LoadError)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(format!("typio-engine-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    const VALID_KEYBOARD_TOML: &str = r#"
name = "demo"
type = "keyboard"
protocol = "typio-engine-protocol"
display_name = "Demo"
command = "/usr/bin/typio-engine-demo"
args = []
required = ["preedit", "candidates"]
optional = ["prediction"]
"#;

    #[test]
    fn load_single_registers_a_valid_keyboard_manifest() {
        let temp = tempdir().unwrap();
        let path = write_manifest(temp.path(), "demo", VALID_KEYBOARD_TOML);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        loader.load_single(&mut registry, &path).expect("loads");

        let keyboards = registry.list_keyboards();
        assert_eq!(keyboards, vec!["demo"]);
    }

    #[test]
    fn load_single_skips_unsupported_protocol() {
        let temp = tempdir().unwrap();
        let body = VALID_KEYBOARD_TOML.replace("typio-engine-protocol", "something-else");
        let path = write_manifest(temp.path(), "demo", &body);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let err = loader.load_single(&mut registry, &path).unwrap_err();
        match err {
            LoadError::Skipped(SkipReason::UnsupportedProtocol(p)) => {
                assert_eq!(p, "something-else");
            }
            other => panic!("expected UnsupportedProtocol skip, got {other:?}"),
        }
        assert!(registry.list_keyboards().is_empty());
    }

    #[test]
    fn load_single_skips_invalid_engine_type() {
        let temp = tempdir().unwrap();
        let body = VALID_KEYBOARD_TOML.replace("\"keyboard\"", "\"ai\"");
        let path = write_manifest(temp.path(), "demo", &body);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let err = loader.load_single(&mut registry, &path).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Skipped(SkipReason::InvalidEngineType(_))
        ));
    }

    #[test]
    fn load_single_skips_when_required_capability_not_provided() {
        let temp = tempdir().unwrap();
        let body = VALID_KEYBOARD_TOML.replace(
            "required = [\"preedit\", \"candidates\"]",
            "required = [\"preedit\", \"candidates\", \"ai_suggest\"]",
        );
        let path = write_manifest(temp.path(), "demo", &body);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let err = loader.load_single(&mut registry, &path).unwrap_err();
        match err {
            LoadError::Skipped(SkipReason::MissingRequiredCapabilities(missing)) => {
                assert_eq!(missing, vec!["ai_suggest".to_string()]);
            }
            other => panic!("expected MissingRequiredCapabilities, got {other:?}"),
        }
    }

    #[test]
    fn load_single_skips_when_already_registered() {
        let temp = tempdir().unwrap();
        let path = write_manifest(temp.path(), "demo", VALID_KEYBOARD_TOML);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        loader
            .load_single(&mut registry, &path)
            .expect("first load");

        // Second load of the same name → AlreadyExists surfaced as Skip.
        let err = loader.load_single(&mut registry, &path).unwrap_err();
        match err {
            LoadError::Skipped(SkipReason::AlreadyRegistered(name)) => {
                assert_eq!(name, "demo");
            }
            other => panic!("expected AlreadyRegistered skip, got {other:?}"),
        }
        // Still only one.
        assert_eq!(registry.list_keyboards().len(), 1);
    }

    #[test]
    fn load_dir_reports_registered_skipped_and_failed() {
        let temp = tempdir().unwrap();

        // Three manifests: one valid, one unsupported protocol, one unparseable.
        // The filenames differ from the inner `name = "demo"` field; the
        // registry registers by the *name field*, so all three manifest
        // bodies share the same name and would collide on a successful load.
        // Only "good" actually loads, so it claims "demo" without conflict.
        write_manifest(temp.path(), "good", VALID_KEYBOARD_TOML);
        let bad_proto = VALID_KEYBOARD_TOML.replace("typio-engine-protocol", "wat");
        write_manifest(temp.path(), "badproto", &bad_proto);
        write_manifest(temp.path(), "broken", "name = "); // invalid TOML

        // A non-manifest file that should be ignored entirely.
        fs::write(temp.path().join("typio-engine-notamanifest.txt"), "noise").unwrap();
        fs::write(temp.path().join("README.md"), "nope").unwrap();

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let report = loader.load_dir(&mut registry, temp.path());

        assert_eq!(report.registered, 1, "one engine registered");
        assert_eq!(report.skipped.len(), 1, "badproto skipped");
        assert_eq!(report.failed.len(), 1, "broken failed to parse");
        // The manifest's `name` field is "demo" (from VALID_KEYBOARD_TOML).
        assert_eq!(registry.list_keyboards(), vec!["demo"]);
    }

    #[test]
    fn load_dir_discovers_icon_theme_path() {
        let temp = tempdir().unwrap();
        write_manifest(temp.path(), "good", VALID_KEYBOARD_TOML);
        fs::create_dir(temp.path().join("icons")).unwrap();

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let _ = loader.load_dir(&mut registry, temp.path());

        let icon_path = loader
            .discovered_icon_theme_path()
            .expect("icon path discovered");
        assert_eq!(icon_path, temp.path().join("icons"));
    }

    #[test]
    fn load_dir_silently_returns_empty_when_directory_missing() {
        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let report = loader.load_dir(&mut registry, Path::new("/nonexistent/typio/engines"));
        assert_eq!(report.registered, 0);
        assert!(report.skipped.is_empty());
        assert!(report.failed.is_empty());
    }

    #[test]
    fn load_dir_does_not_override_first_discovered_icon_path() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        fs::create_dir(first.path().join("icons")).unwrap();
        fs::create_dir(second.path().join("icons")).unwrap();
        write_manifest(first.path(), "a", &VALID_KEYBOARD_TOML.replace("demo", "a"));
        write_manifest(
            second.path(),
            "b",
            &VALID_KEYBOARD_TOML.replace("demo", "b"),
        );

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        let _ = loader.load_dir(&mut registry, first.path());
        let _ = loader.load_dir(&mut registry, second.path());

        // First wins.
        assert_eq!(
            loader.discovered_icon_theme_path(),
            Some(first.path().join("icons").as_path())
        );
    }

    #[test]
    fn languages_are_propagated_to_registry() {
        let temp = tempdir().unwrap();
        let body = r#"
name = "rime"
type = "keyboard"
protocol = "typio-engine-protocol"
command = "/usr/bin/typio-engine-rime"
languages = ["zh", "yue"]
"#;
        let path = write_manifest(temp.path(), "rime", body);

        let mut loader = EngineLoader::new();
        let mut registry = EngineRegistry::new();
        loader.load_single(&mut registry, &path).unwrap();

        let langs = registry.known_languages();
        assert!(langs.contains(&"zh".to_string()));
        assert!(langs.contains(&"yue".to_string()));
    }
}
