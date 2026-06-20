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
//!
//! Run with: `cargo run --bin typio-host-check-libtypio`

use typio::core::engine::{EngineAvailability, EngineError, EngineType};
use typio::core::registry::EngineRegistry;

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
    eprintln!("Known leak to fix later:");
    eprintln!("  - EngineRegistry::set_instance takes `*mut TypioInstance` —");
    eprintln!("    a C-shaped back-pointer for callbacks. This needs replacing");
    eprintln!("    with a Rust closure / trait object in a follow-up so the");
    eprintln!("    Rust host never has to construct TypioInstance.");
}
