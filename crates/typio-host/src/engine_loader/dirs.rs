//! Engine directory resolution (ADR-0025).
//!
//! Replaces `typio_engine_dirs_build` in `src/engine_loader.c`.
//!
//! ## Precedence (highest first)
//!
//! 1. CLI-specified directories (`--engine-dir DIR`, repeatable) — passed
//!    in order.
//! 2. Each colon-separated segment of `$TYPIO_ENGINE_PATH`, in order.
//! 3. The compile-time system directory baked into the build.
//!
//! There is no per-user auto-scan. The daemon auto-loads only from the
//! trusted system directory; every other source is an explicit operator
//! opt-in.
//!
//! ## Compile-time system directory
//!
//! The C version bakes the system engine directory into
//! `typio_build_config.h` at meson configure time, expanded from
//! `<prefix>/<datadir>/typio/engines`. The Rust port gets the same value
//! via the `TYPIO_ENGINE_DIR` environment variable at build time, with a
//! sensible default for `cargo run` from a source checkout. Distros that
//! need a different system path set it via
//! `TYPIO_ENGINE_DIR=/usr/share/typio/engines cargo build`.

use std::path::{Path, PathBuf};

/// The compile-time system engine directory.
///
/// Override at build time with `TYPIO_ENGINE_DIR=...`. The default
/// matches the meson `typio_build_config.h.in` default for a `/usr/local`
/// prefix.
pub const SYSTEM_ENGINE_DIR: &str = match option_env!("TYPIO_ENGINE_DIR") {
    Some(s) => s,
    None => "/usr/local/share/typio/engines",
};

/// Environment variable name for the colon-separated override path.
pub const ENV_ENGINE_PATH: &str = "TYPIO_ENGINE_PATH";

/// Resolve the ordered list of engine directories to scan.
///
/// See the module docs for precedence rules. Empty CLI entries and empty
/// `$TYPIO_ENGINE_PATH` segments are skipped. The system directory is
/// always appended last (even if empty — callers can ignore empty
/// entries).
pub fn resolve_engine_dirs<I, S>(cli_dirs: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<PathBuf> = Vec::new();

    // 1. CLI dirs.
    for d in cli_dirs {
        let s = d.as_ref();
        if !s.is_empty() {
            out.push(PathBuf::from(s));
        }
    }

    // 2. $TYPIO_ENGINE_PATH segments.
    if let Ok(env_path) = std::env::var(ENV_ENGINE_PATH) {
        for segment in env_path.split(':') {
            if !segment.is_empty() {
                out.push(PathBuf::from(segment));
            }
        }
    }

    // 3. Compile-time system directory.
    if !SYSTEM_ENGINE_DIR.is_empty() {
        out.push(PathBuf::from(SYSTEM_ENGINE_DIR));
    }

    out
}

/// Locate the manifest file for a named engine across an ordered list of
/// search directories, returning the path of the first match.
///
/// Mirrors the body of `typio_engine_loader_reload` in the C version that
/// scans `engine_dirs` for `typio-engine-<name>.toml`. Returns `None` if
/// no directory contains a matching file.
pub fn find_manifest_for<'a, I>(engine_dirs: I, name: &str) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let filename = format!("typio-engine-{name}.toml");
    for dir in engine_dirs {
        let candidate = dir.join(&filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that touch `$TYPIO_ENGINE_PATH` need to run serially (env vars
    /// are process-global). This mutex is cheap because the tests that take
    /// it are small and few.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn capture_env() -> Option<String> {
        std::env::var(ENV_ENGINE_PATH).ok()
    }

    fn restore_env(prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var(ENV_ENGINE_PATH, v),
            None => std::env::remove_var(ENV_ENGINE_PATH),
        }
    }

    #[test]
    fn resolve_with_only_system_dir_by_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = capture_env();
        std::env::remove_var(ENV_ENGINE_PATH);

        let dirs = resolve_engine_dirs(Vec::<String>::new());
        assert_eq!(dirs, vec![PathBuf::from(SYSTEM_ENGINE_DIR)]);

        restore_env(prev);
    }

    #[test]
    fn resolve_cli_dirs_come_first() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = capture_env();
        std::env::remove_var(ENV_ENGINE_PATH);

        let dirs = resolve_engine_dirs(["/cli/one", "/cli/two"]);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/cli/one"),
                PathBuf::from("/cli/two"),
                PathBuf::from(SYSTEM_ENGINE_DIR),
            ]
        );

        restore_env(prev);
    }

    #[test]
    fn resolve_env_path_segments_between_cli_and_system() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = capture_env();
        std::env::set_var(ENV_ENGINE_PATH, "/env/one:/env/two");

        let dirs = resolve_engine_dirs(["/cli"]);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/cli"),
                PathBuf::from("/env/one"),
                PathBuf::from("/env/two"),
                PathBuf::from(SYSTEM_ENGINE_DIR),
            ]
        );

        restore_env(prev);
    }

    #[test]
    fn resolve_empty_segments_are_skipped() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = capture_env();
        std::env::set_var(ENV_ENGINE_PATH, ":/env/one::/env/two:");

        let dirs = resolve_engine_dirs(["", "/cli", ""]);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/cli"),
                PathBuf::from("/env/one"),
                PathBuf::from("/env/two"),
                PathBuf::from(SYSTEM_ENGINE_DIR),
            ]
        );

        restore_env(prev);
    }

    #[test]
    fn find_manifest_for_returns_first_match() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("a");
        let dir_b = temp.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_b.join("typio-engine-rime.toml"), b"name = 'rime'").unwrap();

        let dirs = [dir_a.as_path(), dir_b.as_path()];
        let found = find_manifest_for(dirs.iter().copied(), "rime");
        assert_eq!(
            found,
            Some(dir_b.join("typio-engine-rime.toml"))
        );
    }

    #[test]
    fn find_manifest_for_returns_none_when_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let dirs = [dir];
        let found = find_manifest_for(dirs.iter().copied(), "nope");
        assert!(found.is_none());
    }
}
