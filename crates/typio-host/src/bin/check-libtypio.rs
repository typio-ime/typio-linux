//! Phase 0.5 spike: prove typio-host can consume libtypio's NATIVE Rust API
//! (core::registry, core::engine) directly — bypassing the C-shaped
//! TypioInstance / c_api layer entirely.
//!
//! This is the architectural justification for the whole migration: if the
//! Rust host can drive libtypio via native traits (`EngineRegistry`,
//! `Result<T, EngineError>`, real Rust enums), the C ABI becomes purely an
//! engine-plugin contract — not something the host pays for.
//!
//! What this binary does:
//!   1. Constructs an `EngineRegistry` directly (no TypioInstance, no
//!      CString, no raw pointers).
//!   2. Exercises a few read-only methods (list_keyboards, known_languages).
//!   3. Confirms the native error type (`EngineError`) and availability
//!      enum are reachable as Rust types.
//!   4. Constructs a `TypioInstance` via the Rust-native constructor
//!      `TypioInstance::new_rust` (libtypio follow-up to ADR-0035) and
//!      verifies init/shutdown work without ever touching the C ABI
//!      surface.
//!
//! Run with: `cargo run --bin check-libtypio`

use typio::core::engine::{EngineAvailability, EngineError, EngineType};
use typio::core::registry::EngineRegistry;
use typio::instance::TypioInstance;

fn main() {
    eprintln!("typio-host-check-libtypio (Phase 0.5 spike)");
    eprintln!();

    // 1. Construct a registry directly. No CString, no *mut, no extern "C".
    let mut registry = EngineRegistry::new();
    eprintln!("OK:   constructed EngineRegistry directly via native API");

    // 2. Exercise read-only methods to prove the trait surface is real.
    let keyboards = registry.list_keyboards();
    let voices = registry.list_voices();
    let languages = registry.known_languages();
    eprintln!(
        "OK:   initial state: keyboards={:?}, voices={:?}, languages={:?}",
        keyboards, voices, languages
    );

    // 3. Calling activate on an empty registry returns a typed Result,
    //    not an out-param + bool. This is what the migration buys us.
    let err = registry.activate_keyboard("nonexistent").unwrap_err();
    eprintln!(
        "OK:   typed Result returned: activate_keyboard(\"nonexistent\") = {err} (as expected on empty registry)"
    );

    // 4. Native enums are reachable and have Debug/PartialEq.
    let _t: EngineType = EngineType::Keyboard;
    let _a: EngineAvailability = EngineAvailability::Uninitialized;
    let _e: &EngineError = &err;
    eprintln!("OK:   native enums in scope: EngineType, EngineAvailability, EngineError");

    // 5. Rust-native TypioInstance lifecycle (added in the libtypio
    //    follow-up to ADR-0035): no C ABI wrappers needed.
    eprintln!();
    eprintln!("Demonstrating TypioInstance Rust-native constructor:");
    let temp = std::env::temp_dir().join(format!(
        "typio-spike-check-libtypio-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&temp);
    let mut instance = TypioInstance::new_rust(
        temp.to_str(),
        temp.to_str(),
        temp.to_str(),
        Vec::new(),
    );
    eprintln!("OK:   TypioInstance::new_rust allocated");

    // Before init: accessors return None.
    assert!(instance.registry_rust().is_none());
    assert!(instance.config_rust().is_none());

    instance
        .init_rust()
        .expect("TypioInstance::init_rust should succeed");
    eprintln!("OK:   TypioInstance::init_rust ran cleanly");

    // After init: typed accessors return Some.
    let _reg = instance
        .registry_rust()
        .expect("registry_rust should be Some after init");
    let _cfg = instance
        .config_rust()
        .expect("config_rust should be Some after init");
    eprintln!("OK:   registry_rust() + config_rust() return typed refs");

    instance.shutdown_rust();
    eprintln!("OK:   TypioInstance::shutdown_rust persisted state");
    let _ = std::fs::remove_dir_all(&temp);

    eprintln!();
    eprintln!("Phase 0.5 spike succeeded.");
    eprintln!();
    eprintln!("Architectural confirmation:");
    eprintln!("  - libtypio's `core::*` modules ARE the native Rust API");
    eprintln!("  - The C-shaped `TypioInstance` in `instance.rs` is NOT the");
    eprintln!("    primary surface; it is a translation layer over `core::`");
    eprintln!("  - A Rust host can drive libtypio via traits and Result<T, E>,");
    eprintln!("    paying nothing for the C ABI (which remains for engines).");
    eprintln!();
    eprintln!("Rust-native TypioInstance API (Phase 0.5 + libtypio follow-up):");
    eprintln!("  - TypioInstance::new_rust(config_dir, data_dir, state_dir, engine_dirs)");
    eprintln!("  - instance.init_rust() -> Result<(), TypioResult>");
    eprintln!("  - instance.registry_rust() -> Option<&EngineRegistry>");
    eprintln!("  - instance.config_rust() -> Option<&Config>");
    eprintln!("  - instance.shutdown_rust()");
    eprintln!();
    eprintln!("The Rust host can construct + init + use a TypioInstance");
    eprintln!("without going through `extern \"C\"` wrappers for any step.");
}
