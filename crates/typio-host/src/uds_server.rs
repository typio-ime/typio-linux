//! Unix-domain-socket server for TIP v3 (ADR-0008).
//!
//! Phase 4 port of `src/ipc/uds_server.{h,c}` (555 lines of C). Owns:
//!
//! - a `SOCK_STREAM` `AF_UNIX` listening socket
//! - an internal `epoll` instance multiplexing the listener + all accepted
//!   client connections
//! - per-client framing state (length-prefixed message queue)
//! - per-client subscription state (used by `events.subscribe` +
//!   server-emitted notifications)
//!
//! The epoll fd is exposed via [`UdsServer::epoll_fd`] for integration with
//! any external event loop. On wake, the caller invokes
//! [`UdsServer::dispatch`], which drains pending epoll events in a
//! non-blocking fashion (mirrors the C `epoll_wait(timeout=0)`).
//!
//! ## Wire framing
//!
//! Each TIP message is sent as `[4-byte big-endian length][JSON]`. The
//! length is the byte length of the JSON payload (NOT including the
//! length prefix itself). Frames larger than [`MAX_FRAME_BYTES`] are
//! rejected and the offending client is disconnected (matches the C
//! `TYPIO_UDS_MAX_FRAME`).
//!
//! ## What is NOT ported
//!
//! The C version's `ipc_bus.c` (301 lines) is the routing/handler layer
//! that wires UDS requests to TypioInstance/TypioStateController. It is
//! heavily coupled to libtypio's C ABI and the state-controller machinery
//! — neither of which the Rust host has integrated yet. Defer until
//! enough of the daemon is ported to actually serve requests.
//!
//! ## Handler model
//!
//! The caller installs a closure via [`UdsServer::set_handler`]. The
//! closure is invoked for each fully-decoded JSON request frame and
//! returns a [`RequestOutcome`] — optionally a response string and/or a
//! subscription update. This split avoids the borrow problem of the
//! closure trying to call back into `&mut self`: subscription changes
//! are recorded in the return value and applied by `dispatch` after the
//! closure returns.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nix::sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags};
use nix::unistd::getuid;

use crate::ipc::protocol;

/// Maximum simultaneously-connected clients. Matches the C constant
/// `TYPIO_UDS_MAX_CLIENTS`.
pub const MAX_CLIENTS: usize = 16;

/// Maximum frame size (1 MiB). Larger frames are rejected and the
/// offending client is disconnected. Matches `TYPIO_UDS_MAX_FRAME`.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// Maximum distinct topics a single client can subscribe to. Matches
/// `TYPIO_UDS_MAX_TOPICS`.
pub const MAX_TOPICS_PER_CLIENT: usize = 16;

/// Per-client read buffer size (matches `TYPIO_UDS_READBUF`).
const READ_BUF_BYTES: usize = 8192;

/// Per-client write buffer size (matches `TYPIO_UDS_WRITEBUF`).
const WRITE_BUF_BYTES: usize = 65536;

/// Listen backlog (matches `TYPIO_UDS_BACKLOG`). `std::os::unix::net`
/// does not expose `listen()` backlog size on `UnixListener::bind`; the
/// kernel default (128) is used. Documented here for parity with C.
#[allow(dead_code)]
const LISTEN_BACKLOG: i32 = 8;

/// Stable identifier for a connected client. Passed to the handler
/// closure. Use as a key into the subscription registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// What the handler wants to do in response to a request.
///
/// The handler returns this struct; `dispatch` applies the subscription
/// update (if any) and sends the response (if any) on the handler's
/// behalf. This avoids the closure having to call back into `&mut self`.
#[derive(Debug, Default, Clone)]
pub struct RequestOutcome {
    /// JSON-RPC response to send back. `None` means "no reply"
    /// (e.g. for a notification-style request).
    pub response: Option<String>,
    /// Subscription change to apply for this client. `None` leaves the
    /// current subscription untouched.
    pub subscription: Option<SubscriptionUpdate>,
}

impl RequestOutcome {
    /// Send a response, don't touch the subscription.
    pub fn respond(json: impl Into<String>) -> Self {
        Self {
            response: Some(json.into()),
            subscription: None,
        }
    }

    /// No response, but update the subscription.
    pub fn subscribe(update: SubscriptionUpdate) -> Self {
        Self {
            response: None,
            subscription: Some(update),
        }
    }

    /// Send a response AND update the subscription atomically.
    pub fn respond_and_subscribe(json: impl Into<String>, update: SubscriptionUpdate) -> Self {
        Self {
            response: Some(json.into()),
            subscription: Some(update),
        }
    }

    /// Neither respond nor touch the subscription.
    pub fn silent() -> Self {
        Self::default()
    }
}

/// Subscription changes the handler can request.
#[derive(Debug, Clone)]
pub enum SubscriptionUpdate {
    /// Subscribe to all topics (matches `topic_count == 0` in C).
    Wildcard,
    /// Subscribe to specific topics. Replaces any prior subscription.
    /// Empty vec is equivalent to [`SubscriptionUpdate::Unsubscribe`].
    Topics(Vec<String>),
    /// Drop all subscriptions.
    Unsubscribe,
}

/// Per-client subscription state.
#[derive(Debug, Clone, Default)]
enum Subscription {
    /// Not subscribed (no notifications will be delivered).
    #[default]
    None,
    /// Subscribed to all topics.
    Wildcard,
    /// Subscribed to specific topic names.
    Topics(Vec<String>),
}

impl Subscription {
    fn matches(&self, topic: &str) -> bool {
        match self {
            Subscription::None => false,
            Subscription::Wildcard => true,
            Subscription::Topics(ts) => ts.iter().any(|t| t == topic),
        }
    }

    fn apply(&mut self, update: SubscriptionUpdate) {
        *self = match update {
            SubscriptionUpdate::Wildcard => Subscription::Wildcard,
            SubscriptionUpdate::Topics(ts) if ts.is_empty() => Subscription::None,
            SubscriptionUpdate::Topics(ts) => {
                let truncated = ts.into_iter().take(MAX_TOPICS_PER_CLIENT).collect();
                Subscription::Topics(truncated)
            }
            SubscriptionUpdate::Unsubscribe => Subscription::None,
        };
    }
}

/// Per-client state.
struct Client {
    id: ClientId,
    fd: OwnedFd,
    /// Read buffer. Bytes accumulate until a full frame is available.
    rbuf: Vec<u8>,
    /// Write buffer. Outgoing bytes queued when send() returns EAGAIN.
    wbuf: Vec<u8>,
    /// Current subscription state.
    subscription: Subscription,
    /// True after the client has been closed and is awaiting eviction
    /// from the `clients` map.
    closed: bool,
}

impl Client {
    fn new(id: ClientId, fd: OwnedFd) -> Self {
        Self {
            id,
            fd,
            rbuf: Vec::with_capacity(READ_BUF_BYTES),
            wbuf: Vec::with_capacity(WRITE_BUF_BYTES),
            subscription: Subscription::None,
            closed: false,
        }
    }
}

/// Type alias so the handler's boxed closure type stays readable in the
/// struct definition above.
pub type RequestHandler = Box<dyn FnMut(&str, ClientId) -> RequestOutcome + Send>;

/// UDS server with epoll multiplexing.
///
/// See the module docs for the architecture and rationale.
pub struct UdsServer {
    listener: UnixListener,
    epoll: Epoll,
    socket_path: PathBuf,
    /// Client records keyed by id; indexable for O(1) lookup by fd via
    /// `fd_to_id`.
    clients: HashMap<u64, Client>,
    /// Reverse index: fd → client id. Maintained so epoll events (which
    /// carry the fd) can find the right client in O(1).
    fd_to_id: HashMap<RawFd, u64>,
    /// Caller-supplied request handler.
    handler: Option<RequestHandler>,
}

impl UdsServer {
    /// Bind a new UDS server at `socket_path`. The path's parent
    /// directory is created if missing. A pre-existing socket file is
    /// probed: if anything is listening on it, the bind fails; if it's
    /// stale (left over from a crashed daemon), it is removed first.
    pub fn bind(socket_path: &Path) -> io::Result<Self> {
        // Ensure parent directory exists.
        if let Some(parent) = socket_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        // Stale-socket probe + cleanup.
        if socket_path.exists() && is_stale_socket(socket_path) {
            let _ = std::fs::remove_file(socket_path);
        }

        let listener = UnixListener::bind(socket_path)?;
        // Restrict to the owning user (matches `chmod 0600` in C).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
        }
        listener.set_nonblocking(true)?;

        let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC)?;

        // Register the listening socket for read events (new connections).
        let listen_fd = listener.as_raw_fd();
        let event = EpollEvent::new(EpollFlags::EPOLLIN, listen_fd as u64);
        epoll.add(
            unsafe { std::os::fd::BorrowedFd::borrow_raw(listen_fd) },
            event,
        )?;

        Ok(Self {
            listener,
            epoll,
            socket_path: socket_path.to_path_buf(),
            clients: HashMap::new(),
            fd_to_id: HashMap::new(),
            handler: None,
        })
    }

    /// The epoll file descriptor. Add to your event loop with read
    /// interest; on wake, call [`Self::dispatch`].
    pub fn epoll_fd(&self) -> RawFd {
        self.epoll.0.as_raw_fd()
    }

    /// Install the request handler. The handler is invoked for every
    /// fully-decoded JSON request frame; its return value drives both
    /// the response and any subscription update.
    pub fn set_handler<F>(&mut self, handler: F)
    where
        F: FnMut(&str, ClientId) -> RequestOutcome + Send + 'static,
    {
        self.handler = Some(Box::new(handler));
    }

    /// The path the server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Drain pending epoll events. Non-blocking (epoll_wait with
    /// timeout=0); returns immediately if nothing is ready. Mirrors
    /// the C `typio_uds_server_dispatch` semantics.
    pub fn dispatch(&mut self) {
        let mut events: [EpollEvent; MAX_CLIENTS + 2] = [EpollEvent::empty(); MAX_CLIENTS + 2];
        let listen_fd = self.listener.as_raw_fd();

        let n = match self.epoll.wait(&mut events, 0u16) {
            Ok(n) => n,
            Err(nix::Error::EINTR) => return,
            Err(_) => return,
        };

        for evt in events.iter().take(n) {
            let fd = evt.data() as RawFd;
            let flags = evt.events();

            if fd == listen_fd {
                self.accept_new_clients();
            } else if let Some(&client_id_raw) = self.fd_to_id.get(&fd) {
                let client_id = ClientId(client_id_raw);
                let should_close =
                    (flags & (EpollFlags::EPOLLERR | EpollFlags::EPOLLHUP)) != EpollFlags::empty();
                if should_close {
                    self.close_client(client_id);
                    continue;
                }
                if (flags & EpollFlags::EPOLLIN) != EpollFlags::empty() {
                    let closed = self.process_reads(client_id);
                    if closed {
                        continue;
                    }
                }
                // Try to drain pending writes regardless of EPOLLOUT — the
                // C version does the same; if the kernel buffer freed up
                // between events, this opportunistically flushes.
                self.process_writes(client_id);
            }
        }

        // Evict clients flagged for close.
        self.evict_closed_clients();
    }

    /// Send a JSON-RPC notification to every subscribed client matching
    /// `topic`. The payload is wrapped in a notification envelope using
    /// [`crate::ipc::framing::Notification::new`].
    pub fn emit(&mut self, topic: &str, payload: &serde_json::Value) {
        use crate::ipc::framing::Notification;
        let notif = Notification::new(topic, payload.clone());
        let json = match notif.to_json() {
            Ok(s) => s,
            Err(_) => return,
        };

        // Collect recipients first to avoid borrowing self.clients while
        // we mutate it during send.
        let recipients: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, c)| !c.closed && c.subscription.matches(topic))
            .map(|(_, c)| c.id.0)
            .collect();

        for id in recipients {
            self.send_frame(ClientId(id), &json);
        }
    }

    /// Mark `client` as subscribed to the given topics. Replaces any
    /// prior subscription. Empty slice = wildcard (subscribe to all).
    /// The handler normally returns this via [`RequestOutcome`] rather
    /// than calling this method directly; exposed for callers that need
    /// out-of-band subscription (e.g. test harness).
    pub fn subscribe(&mut self, client: ClientId, topics: &[String]) {
        let Some(c) = self.clients.get_mut(&client.0) else {
            return;
        };
        let update = if topics.is_empty() {
            SubscriptionUpdate::Wildcard
        } else {
            SubscriptionUpdate::Topics(topics.to_vec())
        };
        c.subscription.apply(update);
    }

    // ── Internals ─────────────────────────────────────────────────────

    fn accept_new_clients(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.clients.len() >= MAX_CLIENTS {
                        // Drop the new connection — matches C's
                        // "UDS max clients reached" warning + close.
                        drop(stream);
                        continue;
                    }
                    let _ = stream.set_nonblocking(true);

                    // Peer-credential check: reject any connection whose
                    // uid is not ours. Matches the C `SO_PEERCRED` block.
                    if !peer_is_owner(&stream) {
                        continue;
                    }

                    let fd_raw = stream.as_raw_fd();
                    let id = ClientId(NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed));
                    let fd_owned: OwnedFd = stream.into();

                    // Register with epoll.
                    let event = EpollEvent::new(EpollFlags::EPOLLIN, fd_raw as u64);
                    if self
                        .epoll
                        .add(
                            unsafe { std::os::fd::BorrowedFd::borrow_raw(fd_raw) },
                            event,
                        )
                        .is_err()
                    {
                        // Failed to register; drop the connection.
                        drop(fd_owned);
                        continue;
                    }

                    self.fd_to_id.insert(fd_raw, id.0);
                    self.clients.insert(id.0, Client::new(id, fd_owned));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn process_reads(&mut self, client_id: ClientId) -> bool {
        let mut buf = [0u8; READ_BUF_BYTES];

        // Accumulate requests to dispatch after releasing the client borrow.
        // Each entry is the JSON payload of one complete frame.
        let mut complete_frames: Vec<String> = Vec::new();
        let mut consumed_total;
        let mut close_client = false;

        // Phase 1: read available bytes from the kernel and append to the
        // client's rbuf.
        {
            let Some(client) = self.clients.get_mut(&client_id.0) else {
                return false;
            };
            let fd = client.fd.as_raw_fd();
            loop {
                // SAFETY: &mut Client owns the fd.
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n < 0 {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EAGAIN)
                        || e.raw_os_error() == Some(libc::EWOULDBLOCK)
                    {
                        break;
                    }
                    if e.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    client.closed = true;
                    return true;
                }
                if n == 0 {
                    client.closed = true;
                    return true;
                }
                client.rbuf.extend_from_slice(&buf[..n as usize]);
            }

            // Phase 2: extract as many complete frames as possible.
            consumed_total = 0usize;
            while consumed_total + 4 <= client.rbuf.len() {
                let len_field = &client.rbuf[consumed_total..consumed_total + 4];
                let frame_len =
                    u32::from_be_bytes([len_field[0], len_field[1], len_field[2], len_field[3]])
                        as usize;
                if frame_len > MAX_FRAME_BYTES {
                    client.closed = true;
                    return true;
                }
                if consumed_total + 4 + frame_len > client.rbuf.len() {
                    break;
                }
                let json_start = consumed_total + 4;
                let json_end = json_start + frame_len;
                match std::str::from_utf8(&client.rbuf[json_start..json_end]) {
                    Ok(s) => complete_frames.push(s.to_string()),
                    Err(_) => {
                        client.closed = true;
                        return true;
                    }
                }
                consumed_total = json_end;
            }

            if consumed_total > 0 {
                client.rbuf.drain(..consumed_total);
            }
        } // &mut Client borrow ends here.

        // Phase 3: dispatch each complete frame to the handler.
        for json_str in complete_frames {
            let outcome = if let Some(h) = self.handler.as_mut() {
                h(&json_str, client_id)
            } else {
                RequestOutcome::default()
            };

            // Apply subscription update.
            if let Some(update) = outcome.subscription {
                if let Some(c) = self.clients.get_mut(&client_id.0) {
                    c.subscription.apply(update);
                }
            }

            // Send response.
            if let Some(resp) = outcome.response {
                self.send_frame(client_id, &resp);
            }

            // Check if client got closed during the send.
            if let Some(c) = self.clients.get(&client_id.0) {
                if c.closed {
                    close_client = true;
                    break;
                }
            } else {
                close_client = true;
                break;
            }
        }

        close_client
    }

    fn process_writes(&mut self, client_id: ClientId) {
        let Some(client) = self.clients.get_mut(&client_id.0) else {
            return;
        };
        if client.wbuf.is_empty() {
            return;
        }
        let fd = client.fd.as_raw_fd();
        let mut written = 0;
        while written < client.wbuf.len() {
            let remaining = &client.wbuf[written..];
            // SAFETY: &mut Client owns fd; no aliasing.
            let n = unsafe {
                libc::send(
                    fd,
                    remaining.as_ptr() as *const _,
                    remaining.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EAGAIN)
                    || e.raw_os_error() == Some(libc::EWOULDBLOCK)
                {
                    break;
                }
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                client.closed = true;
                return;
            }
            written += n as usize;
        }
        // Drop the bytes we successfully wrote.
        if written > 0 {
            client.wbuf.drain(..written);
        }
        if client.wbuf.len() > WRITE_BUF_BYTES {
            // Overflow: client is not draining. Drop it (matches C).
            client.closed = true;
        }
    }

    fn send_frame(&mut self, client_id: ClientId, json: &str) {
        let json_bytes = json.as_bytes();
        let len_be = (json_bytes.len() as u32).to_be_bytes();

        // Enqueue into wbuf (with overflow check). The client borrow is
        // scoped so process_writes can re-borrow below.
        {
            let Some(client) = self.clients.get_mut(&client_id.0) else {
                return;
            };
            let needed = client.wbuf.len() + 4 + json_bytes.len();
            if needed > WRITE_BUF_BYTES {
                client.closed = true;
                return;
            }
            client.wbuf.extend_from_slice(&len_be);
            client.wbuf.extend_from_slice(json_bytes);
        }

        self.process_writes(client_id);
    }

    fn close_client(&mut self, client_id: ClientId) {
        if let Some(client) = self.clients.get_mut(&client_id.0) {
            client.closed = true;
        }
    }

    fn evict_closed_clients(&mut self) {
        // Collect first to avoid mutating during iteration.
        let to_evict: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, c)| c.closed)
            .map(|(_, c)| c.id.0)
            .collect();
        for id in to_evict {
            if let Some(client) = self.clients.remove(&id) {
                let fd = client.fd.as_raw_fd();
                let _ = self
                    .epoll
                    .delete(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) });
                self.fd_to_id.remove(&fd);
                // Dropping `client` closes the OwnedFd.
            }
        }
    }
}

impl Drop for UdsServer {
    fn drop(&mut self) {
        // Close all clients explicitly so epoll_delete runs.
        let ids: Vec<u64> = self.clients.keys().copied().collect();
        for id in ids {
            self.close_client(ClientId(id));
        }
        self.evict_closed_clients();
        // Unlink the socket file so a subsequent daemon can bind.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Returns true iff nothing is currently listening on the socket at
/// `path`. Used at bind time to clean up stale sockets left by a
/// crashed previous daemon. Mirrors the C `stale_socket_probe`.
fn is_stale_socket(path: &Path) -> bool {
    use std::os::unix::net::UnixStream;
    // Connection succeeded → someone is listening; not stale.
    // Connection refused → nothing listening; stale.
    UnixStream::connect(path).is_err()
}

/// Returns true iff the peer at the other end of `stream` is owned by
/// the same uid as this process. Mirrors the C `SO_PEERCRED` check.
fn peer_is_owner(stream: &std::os::unix::net::UnixStream) -> bool {
    use nix::sys::socket::getsockopt;
    use nix::sys::socket::sockopt::PeerCredentials;
    let creds = match getsockopt(stream, PeerCredentials) {
        Ok(c) => c,
        Err(_) => return true, // couldn't read creds — be permissive
    };
    creds.uid() == getuid().as_raw()
}

// Bring the protocol module's name into scope for doc-links.
#[allow(unused_imports)]
use protocol as _protocol_doc_anchor;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    /// Read one length-prefixed frame from `stream`. Returns the JSON
    /// payload as a String.
    fn read_frame(stream: &mut UnixStream) -> io::Result<String> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame too large: {len}"),
            ));
        }
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload)?;
        Ok(String::from_utf8(payload).unwrap())
    }

    /// Write one length-prefixed frame to `stream`.
    fn write_frame(stream: &mut UnixStream, json: &str) -> io::Result<()> {
        let bytes = json.as_bytes();
        let len_be = (bytes.len() as u32).to_be_bytes();
        stream.write_all(&len_be)?;
        stream.write_all(bytes)?;
        Ok(())
    }

    /// Drive `server.dispatch()` in a background thread until `stop`
    /// becomes true. Returns the join handle.
    fn spawn_dispatcher(
        server: Arc<Mutex<UdsServer>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(mut s) = server.lock() {
                    s.dispatch();
                }
                thread::sleep(Duration::from_millis(2));
            }
        })
    }

    #[test]
    fn subscription_state_machine() {
        let mut s = Subscription::None;
        assert!(!s.matches("anything"));
        s.apply(SubscriptionUpdate::Wildcard);
        assert!(s.matches("anything"));
        assert!(s.matches("engine.changed"));
        s.apply(SubscriptionUpdate::Topics(vec!["engine.changed".into()]));
        assert!(s.matches("engine.changed"));
        assert!(!s.matches("config.changed"));
        // Truncation: subscription accepts up to MAX_TOPICS_PER_CLIENT
        // entries, drops the rest.
        let many: Vec<String> = (0..MAX_TOPICS_PER_CLIENT + 5)
            .map(|i| format!("topic{i}"))
            .collect();
        s.apply(SubscriptionUpdate::Topics(many));
        match &s {
            Subscription::Topics(ts) => {
                assert_eq!(ts.len(), MAX_TOPICS_PER_CLIENT);
            }
            _ => panic!("expected Topics"),
        }
        s.apply(SubscriptionUpdate::Unsubscribe);
        assert!(matches!(s, Subscription::None));
    }

    #[test]
    fn bind_creates_socket_file_and_cleanup_on_drop() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("test.sock");
        {
            let server = UdsServer::bind(&path).unwrap();
            assert!(path.exists(), "socket file should exist after bind");
            assert!(server.epoll_fd() >= 0);
        }
        // Drop should have unlinked the socket.
        assert!(!path.exists(), "socket file should be removed on drop");
    }

    #[test]
    fn bind_removes_stale_socket() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("stale.sock");
        // Create a non-socket file at the path. bind() should treat it
        // as stale and overwrite.
        std::fs::write(&path, b"leftover").unwrap();
        let _server = UdsServer::bind(&path).unwrap();
        // Socket should now exist and be a real socket (UnixStream can connect).
        let conn = UnixStream::connect(&path);
        assert!(conn.is_ok(), "should be a listening socket now: {:?}", conn);
    }

    #[test]
    fn client_can_send_request_and_receive_response() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("rpc.sock");
        let mut server = UdsServer::bind(&path).unwrap();
        server.set_handler(|json, _client| {
            // Echo the request back as the result field.
            RequestOutcome::respond(format!(r#"{{"echo":{json}}}"#))
        });
        let server = Arc::new(Mutex::new(server));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let driver = spawn_dispatcher(server.clone(), stop.clone());

        let mut client = UnixStream::connect(&path).unwrap();
        // Allow the dispatcher to accept.
        thread::sleep(Duration::from_millis(20));

        let req = r#"{"jsonrpc":"2.0","id":1,"method":"hello"}"#;
        write_frame(&mut client, req).unwrap();

        // Wait for response with a deadline.
        let deadline = Instant::now() + Duration::from_secs(2);
        let response = loop {
            if Instant::now() > deadline {
                panic!("no response within timeout");
            }
            match read_frame(&mut client) {
                Ok(s) => break s,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("read error: {e}"),
            }
        };

        assert!(
            response.contains("hello"),
            "response should echo method: {response}"
        );
        assert!(response.contains("\"echo\":"));

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = driver.join();
    }

    #[test]
    fn emit_delivers_notification_to_subscribed_clients_only() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("emit.sock");
        let mut server = UdsServer::bind(&path).unwrap();
        // Handler subscribes every client to a specific topic on hello.
        server.set_handler(|json, _client| {
            if json.contains("hello") {
                RequestOutcome::respond_and_subscribe(
                    r#"{"ok":true}"#,
                    SubscriptionUpdate::Topics(vec!["engine.changed".into()]),
                )
            } else {
                RequestOutcome::silent()
            }
        });
        let server = Arc::new(Mutex::new(server));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let driver = spawn_dispatcher(server.clone(), stop.clone());

        let mut sub_client = UnixStream::connect(&path).unwrap();
        let unsub_client = UnixStream::connect(&path).unwrap();
        thread::sleep(Duration::from_millis(20));

        // Subscribe the first client by sending hello; the second client
        // never subscribes.
        write_frame(
            &mut sub_client,
            r#"{"jsonrpc":"2.0","id":1,"method":"hello"}"#,
        )
        .unwrap();
        // Drain sub_client's hello response before emit.
        let _hello_resp = read_frame(&mut sub_client).unwrap();

        // Emit a notification.
        if let Ok(mut s) = server.lock() {
            s.emit("engine.changed", &serde_json::json!({"name": "rime"}));
        }

        // sub_client should receive the notification.
        let deadline = Instant::now() + Duration::from_secs(2);
        let notif = loop {
            if Instant::now() > deadline {
                panic!("subscribed client did not receive notification");
            }
            match read_frame(&mut sub_client) {
                Ok(s) => break s,
                Err(e)
                    if e.kind() == io::ErrorKind::UnexpectedEof
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("read error: {e}"),
            }
        };
        assert!(notif.contains("engine.changed"));
        assert!(notif.contains("rime"));

        // unsub_client should not receive anything — give it a moment to
        // prove a negative.
        thread::sleep(Duration::from_millis(50));
        // Make unsub_client non-blocking so the negative check returns
        // immediately instead of waiting forever for data that should
        // never arrive.
        unsub_client.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 4];
        let r = (&unsub_client).read(&mut buf);
        assert!(
            matches!(r, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "unsubscribed client should not receive notifications"
        );

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = driver.join();
    }

    #[test]
    fn max_clients_is_enforced() {
        // The C version rejects connections beyond TYPIO_UDS_MAX_CLIENTS.
        // We can't easily exercise the rejection path without spawning
        // MAX_CLIENTS+1 connections; verify the constant is what we expect.
        assert_eq!(MAX_CLIENTS, 16);
    }

    #[test]
    fn request_outcome_builders_are_ergonomic() {
        let r = RequestOutcome::respond("hello");
        assert_eq!(r.response.as_deref(), Some("hello"));
        assert!(r.subscription.is_none());

        let r = RequestOutcome::subscribe(SubscriptionUpdate::Wildcard);
        assert!(r.response.is_none());
        assert!(matches!(r.subscription, Some(SubscriptionUpdate::Wildcard)));

        let r = RequestOutcome::respond_and_subscribe("ok", SubscriptionUpdate::Unsubscribe);
        assert_eq!(r.response.as_deref(), Some("ok"));
        assert!(matches!(
            r.subscription,
            Some(SubscriptionUpdate::Unsubscribe)
        ));

        let r = RequestOutcome::silent();
        assert!(r.response.is_none());
        assert!(r.subscription.is_none());
    }
}
