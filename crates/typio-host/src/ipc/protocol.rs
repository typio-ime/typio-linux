//! TIP v3 protocol constants and socket path resolution.
//!
//! Direct port of `src/ipc/tip_protocol.{h,c}`. Method/topic names are
//! `&'static str` constants matching the wire vocabulary exactly; the
//! `typioctl` client compares against these literals, so renaming a
//! constant here is a wire-incompatible change.

use std::env;
use std::path::PathBuf;

/// Protocol version reported by `hello`. v3 adds the `language.*`
/// namespace, `daemon.status activeLanguage`, and `language.changed`
/// event (ADR-0031).
pub const PROTOCOL_VERSION: u32 = 3;

/// JSON-RPC version string. All TIP messages carry `"jsonrpc": "2.0"`.
pub const JSONRPC_VERSION: &str = "2.0";

/// Wire vocabulary for TIP v3 methods (JSON-RPC 2.0 `method` field).
///
/// Source of truth: ADR-0008 (TIP v1 base), ADR-0026 (modality split),
/// ADR-0031 (language surface).
pub mod methods {
    /// Initial handshake. Returns protocol version + daemon version.
    pub const HELLO: &str = "hello";

    // ── Config surface ────────────────────────────────────────────────
    pub const CONFIG_GET: &str = "config.get";
    pub const CONFIG_SET: &str = "config.set";
    pub const CONFIG_UNSET: &str = "config.unset";
    pub const CONFIG_LIST: &str = "config.list";
    pub const CONFIG_SHOW: &str = "config.show";
    pub const CONFIG_RELOAD: &str = "config.reload";

    // ── Engine surface (cross-modality, keyed by engine name) ─────────
    pub const ENGINE_LIST: &str = "engine.list";
    pub const ENGINE_DESCRIBE: &str = "engine.describe";
    pub const ENGINE_INVOKE: &str = "engine.invoke";
    pub const ENGINE_LOAD: &str = "engine.load";
    pub const ENGINE_UNLOAD: &str = "engine.unload";
    pub const ENGINE_RELOAD: &str = "engine.reload";

    // ── Modality-explicit activation/cycling (ADR-0026) ───────────────
    //
    // Keyboard and voice slots are orthogonal and simultaneously active,
    // so each gets its own verbs rather than a kind-discriminated
    // engine.use/engine.next.
    pub const KEYBOARD_USE: &str = "keyboard.use";
    pub const KEYBOARD_NEXT: &str = "keyboard.next";
    pub const KEYBOARD_PREV: &str = "keyboard.prev";
    pub const VOICE_USE: &str = "voice.use";
    pub const VOICE_NEXT: &str = "voice.next";
    pub const VOICE_PREV: &str = "voice.prev";

    // ── Language surface (ADR-0031) ───────────────────────────────────
    //
    // The active language retargets keyboard+voice together; per-language
    // engine selection is plain config (languages.<tag>.keyboard/.voice).
    pub const LANGUAGE_LIST: &str = "language.list";
    pub const LANGUAGE_USE: &str = "language.use";
    pub const LANGUAGE_NEXT: &str = "language.next";
    pub const LANGUAGE_PREV: &str = "language.prev";

    // ── Daemon lifecycle ──────────────────────────────────────────────
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_STOP: &str = "daemon.stop";
    pub const DAEMON_VERSION: &str = "daemon.version";

    // ── Event subscriptions ───────────────────────────────────────────
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
}

/// Server-to-client notification topics (the `method` field of a
/// JSON-RPC notification pushed by the daemon to subscribed clients).
pub mod topics {
    /// Active engine changed (keyboard or voice slot).
    pub const ENGINE_CHANGED: &str = "engine.changed";
    /// Active language changed (ADR-0031).
    pub const LANGUAGE_CHANGED: &str = "language.changed";
    /// Engine availability lifecycle changed (Uninitialized/Preparing/Ready/Failed).
    pub const ENGINE_STATUS_CHANGED: &str = "engine.statusChanged";
    /// Configuration tree changed (key path, new value).
    pub const CONFIG_CHANGED: &str = "config.changed";
    /// Runtime-derived state changed (focused app, mode, etc.).
    pub const RUNTIME_CHANGED: &str = "runtime.changed";
    /// Daemon is shutting down.
    pub const DAEMON_SHUTDOWN: &str = "daemon.shuttingDown";
}

/// Resolve the canonical UDS socket path.
///
/// Mirrors `typio_ipc_socket_path` in C. Search order:
///   1. `$XDG_RUNTIME_DIR/typio/daemon.sock` (preferred — lifecycle-bound
///      to the user session, auto-cleaned at logout)
///   2. `~/.local/share/typio/daemon.sock` (fallback when XDG_RUNTIME_DIR
///      is unset, e.g. SSH sessions)
///   3. `/tmp/typio-daemon.sock` (last resort)
///
/// Unlike the C version (which returns a freshly-malloc'd string),
/// this returns a [`PathBuf`] by value.
pub fn socket_path() -> PathBuf {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join("typio").join("daemon.sock");
        }
    }
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("typio")
                .join("daemon.sock");
        }
    }
    PathBuf::from("/tmp/typio-daemon.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env-var mutations are process-global — serialise the affected tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn protocol_version_matches_c_tip_v3() {
        // ADR-0031 bumped TIP to v3 (language.* surface). The handshake
        // rejects clients with a different protocolVersion major.
        assert_eq!(PROTOCOL_VERSION, 3);
    }

    #[test]
    fn jsonrpc_version_is_standard_2_0() {
        assert_eq!(JSONRPC_VERSION, "2.0");
    }

    #[test]
    fn method_strings_match_typioctl() {
        // These literals are part of the wire contract; renaming one
        // silently breaks every typioctl release in the wild. Test guards
        // against accidental rename.
        assert_eq!(methods::HELLO, "hello");
        assert_eq!(methods::CONFIG_GET, "config.get");
        assert_eq!(methods::KEYBOARD_NEXT, "keyboard.next");
        assert_eq!(methods::VOICE_PREV, "voice.prev");
        assert_eq!(methods::LANGUAGE_USE, "language.use");
        assert_eq!(methods::DAEMON_STOP, "daemon.stop");
        assert_eq!(methods::EVENTS_SUBSCRIBE, "events.subscribe");
    }

    #[test]
    fn topic_strings_match_typioctl() {
        assert_eq!(topics::ENGINE_CHANGED, "engine.changed");
        assert_eq!(topics::LANGUAGE_CHANGED, "language.changed");
        assert_eq!(topics::DAEMON_SHUTDOWN, "daemon.shuttingDown");
    }

    #[test]
    fn socket_path_prefers_xdg_runtime_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_rt = env::var("XDG_RUNTIME_DIR").ok();
        let prev_home = env::var("HOME").ok();
        env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        env::set_var("HOME", "/home/user");

        let path = socket_path();
        assert_eq!(path, PathBuf::from("/run/user/1000/typio/daemon.sock"));

        match prev_rt {
            Some(v) => env::set_var("XDG_RUNTIME_DIR", v),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
    }

    #[test]
    fn socket_path_falls_back_to_home_share() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_rt = env::var("XDG_RUNTIME_DIR").ok();
        let prev_home = env::var("HOME").ok();
        env::remove_var("XDG_RUNTIME_DIR");
        env::set_var("HOME", "/home/user");

        let path = socket_path();
        assert_eq!(
            path,
            PathBuf::from("/home/user/.local/share/typio/daemon.sock")
        );

        match prev_rt {
            Some(v) => env::set_var("XDG_RUNTIME_DIR", v),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
    }

    #[test]
    fn socket_path_last_resort_is_tmp() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_rt = env::var("XDG_RUNTIME_DIR").ok();
        let prev_home = env::var("HOME").ok();
        env::remove_var("XDG_RUNTIME_DIR");
        env::remove_var("HOME");

        let path = socket_path();
        assert_eq!(path, PathBuf::from("/tmp/typio-daemon.sock"));

        match prev_rt {
            Some(v) => env::set_var("XDG_RUNTIME_DIR", v),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
    }

    #[test]
    fn empty_xdg_runtime_dir_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_rt = env::var("XDG_RUNTIME_DIR").ok();
        let prev_home = env::var("HOME").ok();
        env::set_var("XDG_RUNTIME_DIR", "");
        env::set_var("HOME", "/home/user");

        let path = socket_path();
        assert_eq!(
            path,
            PathBuf::from("/home/user/.local/share/typio/daemon.sock")
        );

        match prev_rt {
            Some(v) => env::set_var("XDG_RUNTIME_DIR", v),
            None => env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
    }
}
