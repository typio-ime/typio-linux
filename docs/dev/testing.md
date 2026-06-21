# Testing

This document is for contributors. It covers how to run and extend the
`typio-linux` test suite.

## Scope

The suite is the Cargo suite in `crates/typio-host`. It covers the shipping
Rust daemon, subsystem ports, TIP framing, UDS IPC, engine discovery, and
headless daemon behavior.

## Run Cargo Tests

Build or refresh sibling dependencies first. These commands run from the
`typio-linux` repository root:

```bash
cargo build --release --manifest-path ../libtypio/Cargo.toml
meson compile -C ../../flux/build    # first time: meson setup ../../flux/build ../../flux
```

Run the full Rust suite:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo test -p typio-host
```

Run one test:

```bash
cargo test -p typio-host service::tests::hello_reports_protocol_and_capabilities
```

Run with output:

```bash
cargo test -p typio-host -- --nocapture
```

If `cargo test` reports an undefined `flux_*` symbol, Cargo loaded a stale
or system `libflux.so`. Rebuild `../../flux`, then confirm
`LD_LIBRARY_PATH` includes `../../flux/build`.

## Cargo Coverage

| Area | Test surface |
|---|---|
| CLI and daemon lifecycle | `app` unit tests, `tests/typio_daemon.rs` |
| TIP protocol and JSON-RPC framing | `ipc` unit tests, `tests/daemon_stub.rs` |
| UDS server and IPC bus | `uds_server`, `ipc_bus`, `service` tests |
| Engine manifests and registration | `engine_loader` unit and integration tests |
| Wayland focus, key policy, repeat, candidate guard | `focus_controller`, `session_glue`, `keyboard_policy`, `keyboard::router`, `candidate_guard` tests |
| Panel policy and text UI state | `panel_scheduler`, `panel_coordinator`, `text_ui_state`, `preedit` tests |
| Tray and status state | `tray_menu`, `tray_sni`, `state_controller`, `language_display`, `icon_badge` tests |
| Runtime support | `config_watcher`, `resume_signal`, `watchdog`, `health`, `pw_capture` tests |

## Add or Update Tests

Add or update tests when changing:

- Wayland lifecycle, key routing, repeat, or startup guard behavior
- runtime config reload, config-watch debounce, or event-loop scheduling
- voice service state transitions, reload deferral, or completion dispatch
- tray action handling or SNI serialization
- candidate Panel layout, rendering, or state classification
- focus-controller `reduce`, `diff`, or guard predicates
- TIP framing, UDS dispatch, or external-input parsing

Prefer small state-policy tests for Wayland behavior. Do not rely only on
manual compositor testing when a bug can be reduced to a helper or state
model.

## Style

- Use Rust tests for host behavior.
- Keep public API names in the style already used by the touched module.
- Prefer local helpers and direct data flow over broad abstractions.
- Document non-obvious behavior near complex state transitions.
- Keep generated protocol and renderer details behind narrow module
  boundaries.
