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

pub mod engine_loader;
pub mod protocols;
