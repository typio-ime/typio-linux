//! Phase 0 spike: prove the Rust toolchain + wayland-client + local
//! wayland-scanner-generated protocol bindings work end-to-end against a
//! live compositor.
//!
//! What this binary does:
//!   1. Connects to the Wayland compositor from WAYLAND_DISPLAY.
//!   2. Snapshots advertised globals and prints the ones relevant to typio.
//!   3. Attempts to bind `zwp_input_method_manager_v2` and
//!      `zwp_virtual_keyboard_manager_v1` — the two protocol entry points
//!      the real host needs.
//!   4. Exits 0 on success, non-zero on failure.
//!
//! This intentionally does NOT implement the input-method lifecycle. It is
//! the smallest binary that answers: "can a Rust host on this machine speak
//! Wayland to the compositor and bind the IM protocols we need?"

mod protocols;

use std::env;
use std::process::ExitCode;

use wayland_client::globals::{registry_queue_init, BindError, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use protocols::input_method_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use protocols::virtual_keyboard_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;

/// Application state. Empty for the spike — we bind and exit, no events to
/// process. In the real host this holds the TypioWlFrontend equivalent.
#[derive(Default)]
struct State;

fn main() -> ExitCode {
    let wayland_display = env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    eprintln!("typio-host (Phase 0 spike): WAYLAND_DISPLAY=\"{wayland_display}\"");

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: cannot connect to Wayland: {e}");
            return ExitCode::from(2);
        }
    };

    let (globals, mut event_queue) = match registry_queue_init::<State>(&conn) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAIL: registry roundtrip failed: {e}");
            return ExitCode::from(3);
        }
    };
    let qh = event_queue.handle();

    print_globals(globals.contents());

    let mut ok = true;

    let im_manager: Result<ZwpInputMethodManagerV2, BindError> = globals.bind(&qh, 1..=1, ());
    match &im_manager {
        Ok(p) => eprintln!("OK:   bound zwp_input_method_manager_v2 v{}", p.version()),
        Err(BindError::NotPresent) => {
            eprintln!("FAIL: compositor does not advertise zwp_input_method_manager_v2");
            eprintln!("      (input-method-v2 is required; check your compositor config)");
            ok = false;
        }
        Err(e) => {
            eprintln!("FAIL: bind zwp_input_method_manager_v2: {e:?}");
            ok = false;
        }
    }

    let vk_manager: Result<ZwpVirtualKeyboardManagerV1, BindError> = globals.bind(&qh, 1..=1, ());
    match &vk_manager {
        Ok(p) => eprintln!(
            "OK:   bound zwp_virtual_keyboard_manager_v1 v{}",
            p.version()
        ),
        Err(BindError::NotPresent) => {
            eprintln!("FAIL: compositor does not advertise zwp_virtual_keyboard_manager_v1");
            eprintln!("      (virtual-keyboard-v1 is required)");
            ok = false;
        }
        Err(e) => {
            eprintln!("FAIL: bind zwp_virtual_keyboard_manager_v1: {e:?}");
            ok = false;
        }
    }

    if let Err(e) = event_queue.roundtrip(&mut State) {
        eprintln!("FAIL: post-bind roundtrip: {e}");
        return ExitCode::from(4);
    }

    if ok {
        eprintln!("Spike succeeded.");
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_globals(contents: &GlobalListContents) {
    let interesting: &[(&str, &str)] = &[
        ("zwp_input_method_manager_v2", "input-method-v2 (required)"),
        (
            "zwp_virtual_keyboard_manager_v1",
            "virtual-keyboard-v1 (required)",
        ),
        ("wl_seat", "seat (required)"),
        ("ext_foreign_toplevel_list_v1", "foreign-toplevel (future)"),
        (
            "wp_fractional_scale_manager_v1",
            "fractional-scale (future)",
        ),
        ("wp_viewporter", "viewporter (future)"),
    ];
    let list = contents.clone_list();
    eprintln!("Global list from compositor ({} advertised):", list.len());
    for (iface, why) in interesting {
        let versions: Vec<u32> = list
            .iter()
            .filter(|g| g.interface == *iface)
            .map(|g| g.version)
            .collect();
        match versions.as_slice() {
            [] => eprintln!("  - {iface:<42} [absent]   {why}"),
            v => eprintln!("  - {iface:<42} v{v:?}  {why}"),
        }
    }
    let other_count = list
        .iter()
        .filter(|g| !interesting.iter().any(|(i, _)| i == &g.interface))
        .count();
    eprintln!("  + {other_count} other globals");
}

// ---- Dispatch impls: the spike binds and exits, so all bodies are no-ops.
// Real host replaces these with actual event handlers.

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
