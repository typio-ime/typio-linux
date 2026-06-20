//! typio-daemon-stub — the first runnable Rust-side daemon binary.
//!
//! Wires together the pieces we have ported so far (engine_loader +
//! uds_server + the TIP framing) into a minimal daemon that can answer
//! `hello` over the UDS control socket. typioctl can connect to it and
//! get a real handshake response back.
//!
//! ## What it does
//!
//! 1. Resolves the engine-dir search path (CLI / `$TYPIO_ENGINE_PATH` /
//!    compile-time system dir).
//! 2. Constructs an `EngineRegistry` and an `EngineLoader`, scans every
//!    engine dir, registers whatever engines are installed (typically
//!    rime/mozc/sherpa when the sibling repos are built).
//! 3. Binds a [`UdsServer`] at `protocol::socket_path()` (default
//!    `$XDG_RUNTIME_DIR/typio/daemon.sock`).
//! 4. Installs a request handler that responds to:
//!    - `hello` → real handshake: protocol version + daemon version +
//!      list of loaded engines
//!    - `daemon.version` → crate version string
//!    - `daemon.status` → basic runtime info (engines loaded, listening
//!      socket path)
//!    - all other methods → JSON-RPC `-32601 Method not found` error
//! 5. Runs a single-threaded poll(2) loop driving the server's epoll_fd.
//!
//! ## What it does NOT do
//!
//! Everything else. No Wayland connection, no input-method binding, no
//! keyboard routing, no config tree, no tray. This is a smoke test for
//! the daemon skeleton — the goal is to prove that a typioctl client
//! gets a real handshake response back from a Rust-built `typio`
//! binary.
//!
//! ## Try it
//!
//! ```sh
//! cargo build --bin typio-daemon-stub
//! ./target/debug/typio-daemon-stub
//! # in another shell:
//! typioctl hello
//! typioctl daemon.status
//! ```

use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};
use typio::core::registry::EngineRegistry;

use typio_host::engine_loader::{dirs, EngineLoader};
use typio_host::ipc::framing::{Message, Request, Response, StandardError};
use typio_host::ipc::protocol::{self, methods, PROTOCOL_VERSION};
use typio_host::uds_server::{RequestOutcome, SubscriptionUpdate, UdsServer};

fn main() -> ExitCode {
    eprintln!("typio-daemon-stub: starting (v{})", env!("CARGO_PKG_VERSION"));

    // 1. Engine discovery + registration.
    let mut registry = EngineRegistry::new();
    let mut loader = EngineLoader::with_voice();
    let engine_dirs = dirs::resolve_engine_dirs(Vec::<String>::new());
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

    // 2. UDS server bind.
    let socket_path = protocol::socket_path();
    eprintln!("typio-daemon-stub: binding UDS at {}", socket_path.display());
    let mut server = match UdsServer::bind(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: bind UDS: {e}");
            return ExitCode::from(1);
        }
    };

    // 3. Handler.
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
    // Drop the registry — the stub handler returns the engine list as
    // data snapshots, it does not consult the live registry per-call.
    drop(registry);
    drop(loader);
    let socket_path_for_handler = socket_path.clone();
    server.set_handler(move |json: &str, _client| -> RequestOutcome {
        let req: Request = match Request::parse(json) {
            Ok(r) => r,
            Err(_) => {
                return RequestOutcome::respond(
                    Response::error(req_id_or_zero(json), -32600, "Invalid Response")
                        .to_json()
                        .unwrap(),
                );
            }
        };
        let keyboards_refs: Vec<&str> = keyboards.iter().map(|s| s.as_str()).collect();
        let voices_refs: Vec<&str> = voices.iter().map(|s| s.as_str()).collect();
        let response = dispatch(&req, &keyboards_refs, &voices_refs, &socket_path_for_handler);
        // `events.subscribe` carries an implicit subscription update.
        let subscription = if req.method == methods::EVENTS_SUBSCRIBE {
            // Wildcard subscription for the stub — every connected client
            // gets every event. A real daemon would parse params.
            Some(SubscriptionUpdate::Wildcard)
        } else {
            None
        };
        match response {
            Some(resp_json) => RequestOutcome {
                response: Some(resp_json),
                subscription,
            },
            None => RequestOutcome {
                response: None,
                subscription,
            },
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
/// - `None` for "no reply" (notification-style requests, none in this stub)
fn dispatch(
    req: &Request,
    keyboards: &[&str],
    voices: &[&str],
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
        // Methods that exist in the protocol but the stub does not
        // implement yet. Return a structured "not implemented" error so
        // typioctl can present a useful message instead of generic
        // "method not found".
        "config.get" | "config.set" | "config.unset" | "config.list"
        | "config.show" | "config.reload"
        | "engine.list" | "engine.describe" | "engine.invoke"
        | "engine.load" | "engine.unload" | "engine.reload"
        | "keyboard.use" | "keyboard.next" | "keyboard.prev"
        | "voice.use" | "voice.next" | "voice.prev"
        | "language.list" | "language.use" | "language.next"
        | "language.prev"
        | "daemon.stop" => Some(
            Response::error(
                id,
                // -32603 Internal Error; reused here as "known method
                // but not yet implemented by the Rust host". A real
                // daemon returns success.
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

// Suppress unused message-import warning for `methods` re-export alias.
#[allow(unused_imports)]
use Message as _MessageDocAnchor;

// Duration is referenced in the docstring for the polling timeout above.
#[allow(dead_code)]
const _ONE_SECOND: Duration = Duration::from_secs(1);
