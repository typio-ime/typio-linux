//! End-to-end test for the typio-daemon-stub: spin it up as a child
//! process, talk to its UDS socket as a client, verify the protocol
//! contract. Complements the unit tests in [`uds_server`] which test
//! the transport layer in isolation; this test exercises the actual
//! JSON-RPC method dispatch.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Per-test unique socket path under /tmp so parallel cargo test runs
/// don't collide. The stub reads `TYPIO_ENGINE_PATH=:` (empty path) and
/// uses our explicit `--socket` flag (we add that capability to the stub
/// via env override).
static TEST_SERIAL: AtomicU64 = AtomicU64::new(0);

struct DaemonGuard {
    child: Child,
    socket: PathBuf,
    // Keep the tempdir alive for as long as the daemon runs — its Drop
    // would otherwise remove the directory containing the UDS socket
    // (and the daemon-stub.stderr log) while the daemon is still bound.
    _temp: tempfile::TempDir,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Best-effort cleanup.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// We can't easily make the stub take a custom --socket flag without
/// modifying it; instead, we override XDG_RUNTIME_DIR to a tempdir and
/// let socket_path() resolve to <tempdir>/typio/daemon.sock.
fn spawn_stub() -> (DaemonGuard, PathBuf) {
    let _serial = TEST_SERIAL.fetch_add(1, Ordering::Relaxed);
    let temp = tempfile::tempdir().expect("tempdir");
    let xdg = temp.path().to_path_buf();
    let socket = xdg.join("typio").join("daemon.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let bin_path = format!("{manifest_dir}/../../target/debug/typio-daemon-stub");
    let log_path = xdg.join("daemon-stub.stderr");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let child = Command::new(&bin_path)
        .env("XDG_RUNTIME_DIR", &xdg)
        // Empty TYPIO_ENGINE_PATH so resolve_engine_dirs returns only
        // the system dir (which has nothing installed in CI).
        .env("TYPIO_ENGINE_PATH", "")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {bin_path}: {e}"));

    let guard = DaemonGuard {
        child,
        socket: socket.clone(),
        _temp: temp,
    };

    // Wait for the socket to appear.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if socket.exists() {
            // Give the stub a moment to finish bind+listen.
            std::thread::sleep(Duration::from_millis(100));
            return (guard, socket);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let stderr_log = std::fs::read_to_string(xdg.join("daemon-stub.stderr"))
        .unwrap_or_default();
    panic!(
        "stub did not bind socket within 5s at {}\n--- stub stderr ---\n{stderr_log}",
        socket.display()
    );
}

fn send_request(stream: &mut UnixStream, id: i64, method: &str, params: Value) {
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let payload = serde_json::to_vec(&req).unwrap();
    let len_be = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len_be).unwrap();
    stream.write_all(&payload).unwrap();
}

fn recv_response(stream: &mut UnixStream) -> Value {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

#[test]
fn hello_returns_protocol_v3_handshake() {
    let (guard, socket) = spawn_stub();
    eprintln!("test: connecting to {}", socket.display());
    let mut stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            // Diagnostic: dump stub stderr so we can see why bind failed.
            let xdg = socket.parent().unwrap().parent().unwrap();
            let log = std::fs::read_to_string(xdg.join("daemon-stub.stderr"))
                .unwrap_or_default();
            panic!(
                "connect {}: {e}\n--- stub stderr ---\n{log}",
                socket.display()
            );
        }
    };
    let _ = guard; // keep alive through the test
    send_request(&mut stream, 1, "hello", json!({"client": "test"}));
    let resp = recv_response(&mut stream);
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    let result = &resp["result"];
    assert_eq!(result["protocolVersion"], 3);
    assert_eq!(result["daemon"], "typio");
    assert!(result["daemonVersion"].is_string());
    assert!(result["loadedEngines"]["keyboard"].is_array());
    assert!(result["loadedEngines"]["voice"].is_array());
}

#[test]
fn daemon_version_returns_crate_version() {
    let (_guard, socket) = spawn_stub();
    let mut stream = UnixStream::connect(&socket).unwrap();
    send_request(&mut stream, 42, "daemon.version", json!({}));
    let resp = recv_response(&mut stream);
    assert_eq!(resp["id"], 42);
    assert_eq!(resp["result"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn daemon_status_reports_running_and_socket() {
    let (_guard, socket) = spawn_stub();
    let mut stream = UnixStream::connect(&socket).unwrap();
    send_request(&mut stream, 7, "daemon.status", json!({}));
    let resp = recv_response(&mut stream);
    let result = &resp["result"];
    assert_eq!(result["running"], true);
    assert_eq!(result["stub"], true);
    assert_eq!(result["socket"], socket.display().to_string());
}

#[test]
fn unknown_method_returns_method_not_found() {
    let (_guard, socket) = spawn_stub();
    let mut stream = UnixStream::connect(&socket).unwrap();
    send_request(&mut stream, 99, "nonsense.method", json!({}));
    let resp = recv_response(&mut stream);
    assert_eq!(resp["error"]["code"], -32601);
    assert_eq!(resp["error"]["message"], "Method not found");
}

#[test]
fn known_but_unimplemented_method_returns_structured_error() {
    let (_guard, socket) = spawn_stub();
    let mut stream = UnixStream::connect(&socket).unwrap();
    send_request(
        &mut stream,
        5,
        "config.get",
        json!({"key": "voice.engine"}),
    );
    let resp = recv_response(&mut stream);
    assert_eq!(resp["error"]["code"], -32603);
    // Error message should mention the method name so users can tell
    // what they tried.
    let msg = resp["error"]["message"].as_str().unwrap();
    assert!(msg.contains("config.get"), "error message should name the method: {msg}");
}

#[test]
fn malformed_json_returns_invalid_request() {
    let (_guard, socket) = spawn_stub();
    let mut stream = UnixStream::connect(&socket).unwrap();
    // Send garbage that's not valid JSON.
    let payload = b"this is not json";
    let len_be = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len_be).unwrap();
    stream.write_all(payload).unwrap();
    let resp = recv_response(&mut stream);
    assert_eq!(resp["error"]["code"], -32600);
}
