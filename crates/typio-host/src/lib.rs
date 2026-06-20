//! typio-host — the Wayland input-method host for Typio (Rust port in progress).
//!
//! Bilingual coexistence with the C daemon in `src/` is governed by ADR-0035.
//! This crate is the future home of the Rust host. During the migration each
//! ported subsystem lives under a module here; the C code in `src/` continues
//! to build and ship until each Rust replacement is verified.
//!
//! ## Current modules
//!
//! - [`engine_loader`] — Phase 1 port of `src/engine_loader.c`. Discovers
//!   `typio-engine-*.toml` manifests, parses them with the `toml` crate,
//!   negotiates capabilities, and registers out-of-process engine backends
//!   with libtypio's `EngineRegistry` via its native Rust API.
//! - [`config_watcher`] — Phase 2 port of the watch mechanism in
//!   `src/wayland/runtime_config.c`. Watches the config directory and
//!   engine-manifest subdirectory via inotify, filters events to the
//!   relevant files, and debounces with a one-shot timerfd. Pure
//!   mechanism — no frontend side effects; the caller receives a
//!   callback when a confirmed reload should fire.

pub mod backoff;
pub mod candidate_guard;
pub mod config_watcher;
pub mod engine_loader;
pub mod input_method;
pub mod ipc;
pub mod keyboard_policy;
pub mod notifier;
pub mod panel;
pub mod protocols;
pub mod repeat_timer;
pub mod resume_signal;
pub mod uds_server;
