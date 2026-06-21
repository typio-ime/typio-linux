//! typio-daemon-stub — the first runnable Rust-side daemon binary.
//!
//! Wires together the pieces we have ported so far (engine_loader +
//! uds_server + the TIP framing) into a minimal daemon that can answer
//! a useful subset of TIP v3 methods over the UDS control socket.
//! typioctl can connect to it and get real handshake responses back.
//!
//! ## What it does
//!
//! 1. Resolves the engine-dir search path (CLI / `$TYPIO_ENGINE_PATH` /
//!    compile-time system dir).
//! 2. Constructs an `EngineRegistry` and an `EngineLoader`, scans every
//!    engine dir, registers whatever engines are installed (typically
//!    rime/mozc/sherpa when the sibling repos are built).
//! 3. Binds a [`UdsServer`] at `protocol::socket_path()` (or
//!    `--socket PATH` if given).
//! 4. Installs a request handler that responds to:
//!    - `hello` → real handshake: protocol version + daemon version +
//!      list of loaded engines
//!    - `daemon.version` → crate version string
//!    - `daemon.status` → basic runtime info (engines loaded, listening
//!      socket path)
//!    - `engine.list` → per-engine details (name, display name,
//!      description, languages, capabilities)
//!    - `engine.describe` (params: `{"name": "..."}`) → full EngineInfo
//!    - `events.subscribe` → wildcard subscription
//!    - all other methods → JSON-RPC `-32601 Method not found` error,
//!      or `-32603` for known-but-unimplemented
//! 5. Runs a single-threaded poll(2) loop driving the server's epoll_fd.
//!
//! ## What it does NOT do
//!
//! No Wayland connection, no input-method binding, no keyboard routing,
//! no config tree, no tray. This is a smoke test for the daemon skeleton
//! — the goal is to prove that a typioctl client gets real handshake +
//! engine list responses back from a Rust-built `typio` binary.
//!
//! ## Try it
//!
//! ```sh
//! cargo build --bin typio-daemon-stub
//! ./target/debug/typio-daemon-stub
//! # in another shell:
//! typioctl hello
//! typioctl engine list
//! typioctl engine describe rime
//! ```
//!
//! Override the socket path (useful for testing):
//!
//! ```sh
//! ./target/debug/typio-daemon-stub --socket /tmp/typio-test.sock
//! ```

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};
use typio::core::registry::EngineRegistry;

use typio_host::engine_loader::{dirs, EngineLoader};
use typio_host::ipc::framing::{Message, Request, Response, StandardError};
use typio_host::ipc::protocol::{self, methods, PROTOCOL_VERSION};
use typio_host::uds_server::{RequestOutcome, SubscriptionUpdate, UdsServer};

fn main() -> ExitCode {
    eprintln!(
        "typio-daemon-stub: starting (v{})",
        env!("CARGO_PKG_VERSION")
    );

    let socket_override = parse_socket_override();
    let engine_dirs_override = parse_engine_dirs_override();

    // 1. Engine discovery + registration.
    let mut registry = EngineRegistry::new();
    let mut loader = EngineLoader::with_voice();
    let engine_dirs: Vec<std::path::PathBuf> = if let Some(d) = engine_dirs_override {
        vec![std::path::PathBuf::from(d)]
    } else {
        dirs::resolve_engine_dirs(Vec::<String>::new())
    };
    eprintln!("typio-daemon-stub: scanning engine dirs: {engine_dirs:?}");
    let mut total_engines = 0usize;
    for dir in &engine_dirs {
        let report = loader.load_dir(&mut registry, dir.as_path());
        total_engines += report.registered;
        if !report.skipped.is_empty() || !report.failed.is_empty() {
            eprintln!(
                "  {}: {} registered, {} skipped, {} failed",
                dir.display(),
                report.registered,
                report.skipped.len(),
                report.failed.len()
            );
        }
    }
    eprintln!(
        "typio-daemon-stub: {} engine(s) loaded (keyboards={:?}, voices={:?})",
        total_engines,
        registry.list_keyboards(),
        registry.list_voices()
    );

    // Precompute engine info snapshot for the handler. Updates after
    // this point (e.g. dynamic language changes per ADR-0034) won't be
    // reflected — the stub loads once at startup.
    let engine_infos = build_engine_info_snapshot(&registry);
    let keyboards: Vec<String> = registry
        .list_keyboards()
        .into_iter()
        .map(String::from)
        .collect();
    let voices: Vec<String> = registry
        .list_voices()
        .into_iter()
        .map(String::from)
        .collect();
    // Drop the registry — the stub serves snapshots, not live queries.
    drop(registry);
    drop(loader);

    // 2. UDS server bind.
    let socket_path = socket_override.unwrap_or_else(protocol::socket_path);
    eprintln!(
        "typio-daemon-stub: binding UDS at {}",
        socket_path.display()
    );
    let mut server = match UdsServer::bind(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: bind UDS: {e}");
            return ExitCode::from(1);
        }
    };

    // 3. Handler.
    let socket_path_for_handler = socket_path.clone();
    server.set_handler(move |json: &str, _client| -> RequestOutcome {
        let req: Request = match Request::parse(json) {
            Ok(r) => r,
            Err(_) => {
                return RequestOutcome::respond(
                    Response::error(req_id_or_zero(json), -32600, "Invalid Request")
                        .to_json()
                        .unwrap(),
                );
            }
        };
        let response = dispatch(
            &req,
            &keyboards,
            &voices,
            &engine_infos,
            &socket_path_for_handler,
        );
        let subscription = if req.method == methods::EVENTS_SUBSCRIBE {
            Some(SubscriptionUpdate::Wildcard)
        } else {
            None
        };
        RequestOutcome {
            response,
            subscription,
        }
    });

    // 4. Drive the server epoll_fd in a tight poll(2) loop.
    eprintln!(
        "typio-daemon-stub: listening on {} (Ctrl+C to exit)",
        socket_path.display()
    );
    let server_fd = server.epoll_fd();
    let mut fds = [libc::pollfd {
        fd: server_fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    loop {
        fds[0].revents = 0;
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 500) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!("typio-daemon-stub: poll error: {e}");
            return ExitCode::from(2);
        }
        if rc > 0 && (fds[0].revents & libc::POLLIN) != 0 {
            server.dispatch();
        }
    }
}

/// Scan `argv` for `--socket PATH`. Returns `Some(path)` if present.
fn parse_socket_override() -> Option<std::path::PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--socket" {
            return args.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = a.strip_prefix("--socket=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Scan `argv` for `--engine-dir PATH`. Returns `Some(path)` if present.
fn parse_engine_dirs_override() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--engine-dir" {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix("--engine-dir=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Build a snapshot of engine infos keyed by engine name.
///
/// The stub serves this snapshot; it does not consult the live registry
/// per-call. A real daemon queries live state to pick up dynamic
/// language changes (ADR-0034).
fn build_engine_info_snapshot(registry: &EngineRegistry) -> HashMap<String, EngineInfoSnapshot> {
    let mut out = HashMap::new();
    for name in registry
        .list_keyboards()
        .into_iter()
        .chain(registry.list_voices())
    {
        if let Some(info) = registry.engine_info(name) {
            out.insert(
                name.to_string(),
                EngineInfoSnapshot {
                    name: info.name.clone(),
                    display_name: info.display_name.clone(),
                    description: info.description.clone(),
                    author: info.author.clone(),
                    icon: info.icon.clone(),
                    language: info.language.clone(),
                    languages: info.languages.clone(),
                    engine_type: match info.engine_type {
                        typio::core::engine::EngineType::Keyboard => "keyboard",
                        typio::core::engine::EngineType::Voice => "voice",
                    }
                    .to_string(),
                    required_capabilities: info.capabilities.required.clone(),
                    optional_capabilities: info.capabilities.optional.clone(),
                },
            );
        }
    }
    out
}

#[derive(Debug, Clone)]
struct EngineInfoSnapshot {
    name: String,
    display_name: String,
    description: String,
    author: String,
    icon: Option<String>,
    language: String,
    languages: Vec<String>,
    engine_type: String,
    required_capabilities: Vec<String>,
    optional_capabilities: Vec<String>,
}

impl EngineInfoSnapshot {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "displayName": self.display_name,
            "description": self.description,
            "author": self.author,
            "icon": self.icon,
            "language": self.language,
            "languages": self.languages,
            "type": self.engine_type,
            "requiredCapabilities": self.required_capabilities,
            "optionalCapabilities": self.optional_capabilities,
        })
    }
}

/// Parse just the `id` field from raw JSON for use in error responses
/// where we cannot parse the full Request. Returns 0 if absent or
/// unparseable.
fn req_id_or_zero(json: &str) -> i64 {
    let value: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    value.get("id").and_then(Value::as_i64).unwrap_or(0)
}

/// Dispatch a parsed request to a stub handler. Returns:
/// - `Some(json_string)` to send this response back
/// - `None` for "no reply"
fn dispatch(
    req: &Request,
    keyboards: &[String],
    voices: &[String],
    engine_infos: &HashMap<String, EngineInfoSnapshot>,
    socket_path: &std::path::Path,
) -> Option<String> {
    let id = req.id.clone();
    match req.method.as_str() {
        methods::HELLO => {
            let result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "daemonVersion": env!("CARGO_PKG_VERSION"),
                "daemon": "typio",
                "capabilities": ["events"],
                "loadedEngines": {
                    "keyboard": keyboards,
                    "voice": voices,
                },
            });
            Some(Response::success(id, result).to_json().unwrap())
        }
        methods::DAEMON_VERSION => Some(
            Response::success(id, json!(env!("CARGO_PKG_VERSION")))
                .to_json()
                .unwrap(),
        ),
        methods::DAEMON_STATUS => {
            let result = json!({
                "running": true,
                "stub": true,
                "socket": socket_path.display().to_string(),
                "loadedEngines": {
                    "keyboard": keyboards,
                    "voice": voices,
                },
            });
            Some(Response::success(id, result).to_json().unwrap())
        }
        methods::ENGINE_LIST => {
            // Return an array of full engine infos, split by type.
            let keyboards_detail: Vec<Value> = keyboards
                .iter()
                .filter_map(|n| engine_infos.get(n).map(EngineInfoSnapshot::to_json))
                .collect();
            let voices_detail: Vec<Value> = voices
                .iter()
                .filter_map(|n| engine_infos.get(n).map(EngineInfoSnapshot::to_json))
                .collect();
            let result = json!({
                "keyboard": keyboards_detail,
                "voice": voices_detail,
            });
            Some(Response::success(id, result).to_json().unwrap())
        }
        methods::ENGINE_DESCRIBE => {
            let name = req
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str);
            match name {
                Some(n) => match engine_infos.get(n) {
                    Some(info) => {
                        Some(Response::success(id, info.to_json()).to_json().unwrap())
                    }
                    None => Some(
                        Response::error(
                            id,
                            1, // application-defined: engine not found
                            format!("engine `{n}` is not loaded"),
                        )
                        .to_json()
                        .unwrap(),
                    ),
                },
                None => Some(
                    Response::error(id, -32602, "missing required param `name`")
                        .to_json()
                        .unwrap(),
                ),
            }
        }
        // Methods that exist in the protocol but the stub does not
        // implement yet. Return a structured "not implemented" error so
        // typioctl can present a useful message instead of generic
        // "method not found".
        "config.get" | "config.set" | "config.unset" | "config.list"
        | "config.show" | "config.reload"
        | "engine.invoke" | "engine.load" | "engine.unload"
        | "engine.reload"
        | "keyboard.use" | "keyboard.next" | "keyboard.prev"
        | "voice.use" | "voice.next" | "voice.prev"
        | "language.list" | "language.use" | "language.next"
        | "language.prev"
        | "daemon.stop" => Some(
            Response::error(
                id,
                // -32603 Internal Error; reused here as "known method
                // but not yet implemented by the Rust host".
                -32603,
                format!(
                    "method `{}` is part of TIP v{PROTOCOL_VERSION} but not yet implemented by typio-daemon-stub",
                    req.method
                ),
            )
            .to_json()
            .unwrap(),
        ),
        // Unknown method.
        _ => Some(
            Response::error(
                id,
                StandardError::MethodNotFound.code(),
                StandardError::MethodNotFound.message(),
            )
            .to_json()
            .unwrap(),
        ),
    }
}

// Bring the protocol module's name into scope so the doc link above
// resolves.
#[allow(unused_imports)]
use protocol as _protocol_doc_anchor;
#[allow(unused_imports)]
use Message as _MessageDocAnchor;

// Duration is referenced in the docstring for the polling timeout above.
#[allow(dead_code)]
const _ONE_SECOND: Duration = Duration::from_secs(1);
