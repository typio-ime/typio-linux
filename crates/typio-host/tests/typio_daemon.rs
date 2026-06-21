//! Headless integration test for the real `typio` daemon binary.
//!
//! This does not require a Wayland compositor or a D-Bus session; it only
//! verifies that the shipping daemon starts, binds its UDS socket, speaks
//! the JSON-RPC protocol, and shuts down cleanly in response to `daemon.stop`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static TEST_SERIAL: AtomicU64 = AtomicU64::new(0);

struct DaemonGuard {
    child: Child,
    socket: PathBuf,
    _temp: tempfile::TempDir,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn typio_bin_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("../../target/debug/typio")
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("../../target/debug/typio"))
}

fn spawn_typio(extra_args: &[&str]) -> (DaemonGuard, PathBuf) {
    let _serial = TEST_SERIAL.fetch_add(1, Ordering::Relaxed);
    let temp = tempfile::tempdir().expect("tempdir");
    let xdg = temp.path().to_path_buf();
    let socket = xdg.join("typio").join("daemon.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();

    let log_path = xdg.join("typio.stderr");
    let log_file = std::fs::File::create(&log_path).unwrap();

    let mut cmd = Command::new(typio_bin_path());
    cmd.env("XDG_RUNTIME_DIR", &xdg)
        .env("TYPIO_ENGINE_PATH", "")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file));
    for arg in extra_args {
        cmd.arg(arg);
    }

    let child = cmd.spawn().unwrap_or_else(|e| {
        panic!("spawn typio: {e}");
    });

    let guard = DaemonGuard {
        child,
        socket: socket.clone(),
        _temp: temp,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if socket.exists() {
            std::thread::sleep(Duration::from_millis(100));
            return (guard, socket);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let stderr_log = std::fs::read_to_string(xdg.join("typio.stderr")).unwrap_or_default();
    panic!(
        "typio did not bind socket within 5s at {}\n--- typio stderr ---\n{stderr_log}",
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
fn version_flag_is_headless() {
    let output = Command::new(typio_bin_path())
        .arg("--version")
        .output()
        .expect("spawn typio --version");
    assert!(output.status.success(), "typio --version failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "stdout: {stdout}");
}

#[test]
fn help_flag_is_headless() {
    let output = Command::new(typio_bin_path())
        .arg("--help")
        .output()
        .expect("spawn typio --help");
    assert!(output.status.success(), "typio --help failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Typio Wayland input-method daemon"),
        "stdout: {stdout}"
    );
}

#[test]
fn daemon_starts_and_stops_cleanly_headless() {
    let (mut guard, socket) = spawn_typio(&[]);
    let mut stream = UnixStream::connect(&socket).unwrap();

    send_request(&mut stream, 1, "hello", json!({"client": "test"}));
    let hello = recv_response(&mut stream);
    assert_eq!(hello["jsonrpc"], "2.0");
    assert_eq!(hello["id"], 1);
    let result = &hello["result"];
    assert_eq!(result["protocolVersion"], 3);
    assert!(result["daemonVersion"].is_string());
    let caps = result["capabilities"].as_array().expect("capabilities array");
    assert!(caps.iter().any(|v| v == "engine"));
    assert!(caps.iter().any(|v| v == "daemon"));

    send_request(&mut stream, 2, "daemon.stop", json!({}));
    let stop = recv_response(&mut stream);
    assert_eq!(stop["jsonrpc"], "2.0");
    assert_eq!(stop["id"], 2);
    assert!(stop["result"].is_object());

    // The daemon should exit soon after the stop callback fires.
    let deadline = Instant::now() + Duration::from_secs(5);
    let exit_status = loop {
        if let Some(status) = guard.child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            panic!("typio did not exit after daemon.stop");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit_status.success(), "typio exited with {exit_status}");
}

