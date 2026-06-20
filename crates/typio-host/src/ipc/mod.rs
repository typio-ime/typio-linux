//! Typio IPC Protocol (TIP) v3 — the daemon's UDS control surface.
//!
//! Phase 3b port of `src/ipc/tip_protocol.{h,c}` and `src/ipc/tip_json.{h,c}`
//! (520 lines of hand-rolled JSON in C). Replaces the hand-rolled parser
//! and builder with `serde_json` for round-tripping and `serde` derives for
//! the JSON-RPC 2.0 envelope.
//!
//! ## Scope
//!
//! This module covers:
//!
//! - **Protocol constants** (method names, topic names, version) — port of
//!   `tip_protocol.h`. See [`protocol`].
//! - **JSON-RPC 2.0 envelope** (Request, Response, Notification, Error,
//!   Id) — port of `tip_json.h`'s high-level framing helpers. See
//!   [`framing`].
//! - **Socket path resolution** (`$XDG_RUNTIME_DIR/typio/daemon.sock`
//!   first, `~/.local/share/typio/daemon.sock` fallback,
//!   `/tmp/typio-daemon.sock` last resort). See [`protocol::socket_path`].
//!
//! Per-method typed request/response structs (e.g. `HelloParams`,
//! `ConfigGetResult`) are intentionally NOT ported yet — the C version
//! uses ad-hoc `params`/`result` JSON values and the daemon dispatches
//! per-method. Those typed structs will be added when the corresponding
//! handler subsystem is ported (config access, engine registry
//! queries, etc.).
//!
//! ## Layout
//!
//! - [`protocol`] — constants + socket path
//! - [`framing`] — JSON-RPC 2.0 envelope types

pub mod framing;
pub mod protocol;
