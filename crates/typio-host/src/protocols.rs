//! Wayland protocol bindings, generated at compile time from the local XMLs
//! in `protocols/` at the typio-linux repo root.
//!
//! We do not depend on the `wayland-protocols` crate. typio-linux ships the
//! XMLs it needs and generates Rust bindings from them — the same pattern
//! the C code uses with the C `wayland-scanner` tool, against the same XMLs.
//! This keeps a single source of truth for protocol definitions across the
//! bilingual host.
//!
//! ## Layout
//!
//! Each public mod is one wire protocol. The generated types live directly
//! in that mod (NOT under a `client::` submodule — that's a convention the
//! `wayland_protocol!` wrapper macro in wayland-protocols crate adds; we
//! invoke `generate_client_code!` directly so types come straight into the
//! containing mod).
//!
//! | module                | XML                                        | bound by host? |
//! |-----------------------|--------------------------------------------|----------------|
//! | `text_input_v3`       | text-input-unstable-v3.xml                 | no — codegen   |
//! |                       |                                            |   dependency   |
//! |                       |                                            |   of input_    |
//! |                       |                                            |   method_v2    |
//! | `input_method_v2`     | input-method-unstable-v2.xml               | yes (required) |
//! | `virtual_keyboard_v1` | virtual-keyboard-unstable-v1.xml           | yes (required) |
//! | `foreign_toplevel_v1` | ext-foreign-toplevel-list-v1.xml           | future         |
//! | `fractional_scale_v1` | fractional-scale-v1.xml                    | future         |
//! | `viewporter`          | viewporter.xml                             | yes (panel)    |
//!
//! XML paths are relative to this crate's manifest dir
//! (`typio-linux/crates/typio-host`), so `../../protocols/<name>.xml`
//! reaches `typio-linux/protocols/`.

// The imports inside each protocol mod are emitted unconditionally by
// mirroring the wayland_protocol! macro in the wayland-protocols crate;
// whether they're consumed depends on what each XML references (e.g. an
// XML that references wl_surface pulls from wayland_client::protocol::*,
// one that doesn't, doesn't). Silence the resulting unused-import noise
// the same way the wayland-protocols crate does.
#![allow(unused_imports)]
#![allow(dead_code)] // future protocols are wired but not yet consumed

// ── text-input-v3 (codegen dependency of input-method-v2) ────────────────
// input-method-v2.xml references zwp_text_input_v3.{change_cause,content_hint,
// content_purpose} enums in its event arg types. The Rust scanner emits
// strongly-typed enum refs, so we must generate text-input-v3 bindings here
// even though the host never binds a zwp_text_input_manager_v3 global.
pub mod text_input_v3 {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/text-input-unstable-v3.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/text-input-unstable-v3.xml");
}

// ── input-method-v2 (required) ───────────────────────────────────────────
pub mod input_method_v2 {
    use wayland_client;
    use wayland_client::protocol::*;
    // Bring zwp_text_input_v3 enums into scope — input-method-v2.xml's
    // arg types reference them (change_cause / content_hint / content_purpose).
    use super::text_input_v3::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/input-method-unstable-v2.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/input-method-unstable-v2.xml");
}

// ── virtual-keyboard-v1 (required) ───────────────────────────────────────
pub mod virtual_keyboard_v1 {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/virtual-keyboard-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/virtual-keyboard-unstable-v1.xml");
}

// ── future protocols (codegen wired, not yet bound by the spike) ─────────

pub mod foreign_toplevel_v1 {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/ext-foreign-toplevel-list-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/ext-foreign-toplevel-list-v1.xml");
}

pub mod fractional_scale_v1 {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/fractional-scale-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/fractional-scale-v1.xml");
}

pub mod viewporter {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../protocols/viewporter.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../../protocols/viewporter.xml");
}
