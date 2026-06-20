//! Engine manifest (`typio-engine-*.toml`) parsing and path resolution.
//!
//! Replaces the hand-rolled line-based parser in `src/engine_loader.c`. The
//! manifest format was always valid TOML (no sections, flat key=value with
//! quoted strings and bracketed arrays — see any real manifest under
//! `typio-engine-*/build/*.toml`), so we parse with the `toml` crate directly
//! instead of re-implementing a TOML subset.
//!
//! ## Field semantics (matches ADR-0030 / ADR-0031)
//!
//! - `name`, `type`, `protocol`, `command` are required.
//! - `language` is the legacy single-language key (still supported).
//! - `languages` (ordered, primary first) wins over `language` when both
//!   are present (ADR-0031).
//! - `arg` is the single-argument form; `args` is the array form. Both can
//!   appear in the same manifest and concatenate in that order.
//! - `required` and `optional` are capability lists negotiated against the
//!   host's static capability set (see [`super::caps`]).
//!
//! ## Path resolution
//!
//! `command` and each entry of `arg`/`args` may be:
//! - absolute (`/usr/lib/typio/typio-engine-rime`) → used as-is
//! - relative with a slash (`./typio-engine-rime`, `lib/helper.sh`) →
//!   resolved against the manifest's parent directory
//! - a bare name (`typio-engine-rime`) → used as-is (looked up in `$PATH`)
//!
//! The same rules as the C parser, lifted into a pure function on
//! [`resolve_path_arg`].

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Default language code used when a manifest declares neither `language`
/// nor `languages`. Matches the C constant.
pub const DEFAULT_LANGUAGE: &str = "und";

/// Parsed representation of a `typio-engine-*.toml` manifest.
///
/// All optional fields are kept as `Option<T>` so the caller can apply
/// defaults at registration time (e.g. `display_name` falls back to `name`).
/// The `arg` / `args` split mirrors the manifest syntax; use
/// [`EngineManifest::argv`] to get the resolved argument vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineManifest {
    /// Machine-readable engine identifier (e.g. "rime"). Required.
    pub name: String,
    /// Engine category: `"keyboard"` or `"voice"`. Required.
    #[serde(rename = "type")]
    pub engine_type: String,
    /// Wire protocol the engine process speaks. Must be `"typio-engine-protocol"`.
    pub protocol: String,
    /// Binary to exec (may be relative to the manifest directory).
    pub command: Option<String>,
    /// Human-readable name. Falls back to [`EngineManifest::name`] if absent.
    pub display_name: Option<String>,
    /// Free-form description.
    pub description: Option<String>,
    /// Author attribution.
    pub author: Option<String>,
    /// Icon name (freedesktop icon theme spec) or relative path under the
    /// engine directory.
    pub icon: Option<String>,
    /// Legacy single-language declaration. Superseded by `languages` when
    /// both are present (ADR-0031).
    pub language: Option<String>,
    /// Ordered language list, primary first. Wins over `language`.
    pub languages: Option<Vec<String>>,
    /// Single-argument form. Concatenated before `args` if both present.
    pub arg: Option<String>,
    /// Array-form arguments.
    pub args: Option<Vec<String>>,
    /// Capabilities the host MUST provide for this engine to load.
    pub required: Option<Vec<String>>,
    /// Capabilities the engine can use if the host provides them.
    pub optional: Option<Vec<String>>,
}

impl EngineManifest {
    /// Parse a manifest from raw TOML text.
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_text)
    }

    /// Read and parse a manifest from disk.
    pub fn read_from(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ManifestError::Read(path.to_path_buf(), e))?;
        Self::parse(&text).map_err(|e| ManifestError::Parse(path.to_path_buf(), e))
    }

    /// True iff all four required fields are non-empty.
    ///
    /// Required: `name`, `type`, `protocol`, `command`.
    pub fn has_required_fields(&self) -> bool {
        !self.name.is_empty()
            && !self.engine_type.is_empty()
            && !self.protocol.is_empty()
            && self.command.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// Resolve and return the full argv for the engine process: the command
    /// followed by every argument from `arg` and `args`, with each entry
    /// path-resolved against the manifest's directory.
    ///
    /// Returns `Err` if `command` is missing — the caller should treat that
    /// as a manifest-validation failure (the manifest is malformed even if
    /// syntactically valid TOML).
    pub fn argv(&self, manifest_path: &Path) -> Result<Vec<String>, ManifestError> {
        let command = self
            .command
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ManifestError::MissingCommand(manifest_path.to_path_buf()))?;
        let mut out = Vec::with_capacity(1 + self.args.as_ref().map_or(0, Vec::len));
        out.push(resolve_path_arg(manifest_path, command));
        if let Some(arg) = self.arg.as_deref() {
            if !arg.is_empty() {
                out.push(resolve_path_arg(manifest_path, arg));
            }
        }
        if let Some(args) = self.args.as_ref() {
            for a in args {
                if !a.is_empty() {
                    out.push(resolve_path_arg(manifest_path, a));
                }
            }
        }
        Ok(out)
    }

    /// Ordered language list, primary first. Falls back to a single-element
    /// list from the legacy `language` field, or `[DEFAULT_LANGUAGE]` if
    /// neither is present.
    pub fn effective_languages(&self) -> Vec<String> {
        if let Some(langs) = self.languages.as_ref() {
            if !langs.is_empty() {
                return langs.clone();
            }
        }
        if let Some(lang) = self.language.as_ref().filter(|s| !s.is_empty()) {
            return vec![lang.clone()];
        }
        vec![DEFAULT_LANGUAGE.to_string()]
    }

    /// Primary language (first of [`Self::effective_languages`]).
    pub fn primary_language(&self) -> String {
        self.effective_languages()
            .into_iter()
            .next()
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
    }
}

/// Errors that can occur while reading a manifest.
#[derive(Debug)]
pub enum ManifestError {
    /// Could not read the file from disk.
    Read(PathBuf, std::io::Error),
    /// File content is not valid TOML or does not match the manifest schema.
    Parse(PathBuf, toml::de::Error),
    /// Required `command` field is missing or empty.
    MissingCommand(PathBuf),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Read(p, e) => {
                write!(f, "cannot read manifest {}: {e}", p.display())
            }
            ManifestError::Parse(p, e) => {
                write!(f, "manifest {} is not valid TOML: {e}", p.display())
            }
            ManifestError::MissingCommand(p) => {
                write!(
                    f,
                    "manifest {} is missing required `command` field",
                    p.display()
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Resolve a manifest path argument (the value of `command`, `arg`, or one
/// element of `args`) to a concrete path string.
///
/// Rules (mirror `resolve_manifest_arg` in `src/engine_loader.c`):
/// - Absolute path (starts with `/`) → returned as-is.
/// - Contains a path separator (`/`) → joined to the manifest's parent dir.
/// - Bare name → returned as-is (resolved by the OS via `$PATH` at exec).
pub fn resolve_path_arg(manifest_path: &Path, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.starts_with('/') {
        return value.to_string();
    }
    if value.contains('/') {
        let dir = manifest_path.parent().unwrap_or_else(|| Path::new(""));
        return dir.join(value).to_string_lossy().into_owned();
    }
    value.to_string()
}

/// True iff `name` looks like a `typio-engine-*.toml` filename (the only
/// filenames the loader recognises as manifests in an engine directory).
pub fn is_manifest_filename(name: &str) -> bool {
    name.starts_with("typio-engine-") && name.ends_with(".toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
name = "rime"
type = "keyboard"
protocol = "typio-engine-protocol"
display_name = "Rime"
description = "Chinese input engine."
author = "Typio"
icon = "typio-rime-symbolic"
language = "zh"
languages = ["zh", "yue"]
command = "./typio-engine-rime"
args = ["--user-data", "./data"]
required = ["preedit", "candidates"]
optional = ["prediction", "learning"]
"#;

    #[test]
    fn parse_sample_manifest_all_fields() {
        let m = EngineManifest::parse(SAMPLE_TOML).expect("valid TOML");
        assert_eq!(m.name, "rime");
        assert_eq!(m.engine_type, "keyboard");
        assert_eq!(m.protocol, "typio-engine-protocol");
        assert_eq!(m.display_name.as_deref(), Some("Rime"));
        assert_eq!(m.icon.as_deref(), Some("typio-rime-symbolic"));
        assert_eq!(m.language.as_deref(), Some("zh"));
        assert_eq!(m.languages.as_deref(), Some(&["zh".to_string(), "yue".to_string()][..]));
        assert_eq!(m.command.as_deref(), Some("./typio-engine-rime"));
        assert_eq!(
            m.args.as_deref(),
            Some(&["--user-data".to_string(), "./data".to_string()][..])
        );
        assert_eq!(
            m.required.as_deref(),
            Some(&["preedit".to_string(), "candidates".to_string()][..])
        );
        assert_eq!(
            m.optional.as_deref(),
            Some(&["prediction".to_string(), "learning".to_string()][..])
        );
    }

    #[test]
    fn has_required_fields_rejects_missing_command() {
        let mut m = EngineManifest::parse(SAMPLE_TOML).unwrap();
        assert!(m.has_required_fields());
        m.command = None;
        assert!(!m.has_required_fields());
        m.command = Some(String::new());
        assert!(!m.has_required_fields());
    }

    #[test]
    fn argv_resolves_relative_command_and_args() {
        let m = EngineManifest::parse(SAMPLE_TOML).unwrap();
        let manifest_path = Path::new("/etc/typio/engines/typio-engine-rime.toml");
        let argv = m.argv(manifest_path).expect("command present");
        assert_eq!(argv[0], "/etc/typio/engines/./typio-engine-rime");
        assert_eq!(argv[1], "--user-data");
        assert_eq!(argv[2], "/etc/typio/engines/./data");
    }

    #[test]
    fn argv_keeps_absolute_and_bare_names_untouched() {
        let toml = r#"
name = "x"
type = "keyboard"
protocol = "typio-engine-protocol"
command = "/usr/lib/typio/x"
args = ["bare-name", "/abs/path", "./rel"]
"#;
        let m = EngineManifest::parse(toml).unwrap();
        let argv = m.argv(Path::new("/engines/typio-engine-x.toml")).unwrap();
        assert_eq!(argv[0], "/usr/lib/typio/x");
        assert_eq!(argv[1], "bare-name");
        assert_eq!(argv[2], "/abs/path");
        assert_eq!(argv[3], "/engines/./rel");
    }

    #[test]
    fn argv_concatenates_arg_then_args() {
        let toml = r#"
name = "x"
type = "keyboard"
protocol = "typio-engine-protocol"
command = "x"
arg = "--first"
args = ["--second", "--third"]
"#;
        let m = EngineManifest::parse(toml).unwrap();
        let argv = m.argv(Path::new("x.toml")).unwrap();
        assert_eq!(argv, vec!["x", "--first", "--second", "--third"]);
    }

    #[test]
    fn argv_missing_command_is_error() {
        let toml = r#"
name = "x"
type = "keyboard"
protocol = "typio-engine-protocol"
"#;
        let m = EngineManifest::parse(toml).unwrap();
        let err = m.argv(Path::new("x.toml")).unwrap_err();
        assert!(matches!(err, ManifestError::MissingCommand(_)));
    }

    #[test]
    fn effective_languages_prefers_languages_over_legacy_language() {
        let mut m = EngineManifest::parse(SAMPLE_TOML).unwrap();
        assert_eq!(m.effective_languages(), vec!["zh", "yue"]);
        assert_eq!(m.primary_language(), "zh");
        m.languages = Some(vec![]);
        assert_eq!(m.effective_languages(), vec!["zh"]); // falls back
        m.language = None;
        assert_eq!(m.effective_languages(), vec![DEFAULT_LANGUAGE]); // default
    }

    #[test]
    fn is_manifest_filename_matches_only_valid_pattern() {
        assert!(is_manifest_filename("typio-engine-rime.toml"));
        assert!(is_manifest_filename("typio-engine-foo.toml"));
        assert!(!is_manifest_filename("typio-engine.toml")); // missing middle
        assert!(!is_manifest_filename("typio-engine-rime.txt"));
        assert!(!is_manifest_filename("rime.toml"));
        assert!(!is_manifest_filename("typio-engine-rime.toml.bak"));
    }
}
