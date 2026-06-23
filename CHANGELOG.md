# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-06-23

### Fixed

- **`xtask install` now places icons under the `hicolor` theme level.**
  The walk previously started inside `data/icons/hicolor/`, so the
  relative path lost the `hicolor` segment and icons landed at
  `<prefix>/share/icons/scalable/apps/typio-*.svg`. That path is not on
  the freedesktop icon-theme search path, so Gtk/Qt tray lookups
  silently failed at runtime. The walk now starts at `data/icons/` so
  `hicolor/scalable/apps/…svg` is preserved end-to-end and the install
  lands at `<prefix>/share/icons/hicolor/scalable/apps/…svg`.

- **`flux-sys`, `flux-text-sys`, `libtypio`, and `typio-abi` are now git
  dependencies instead of sibling-checkout path dependencies.** The previous
  `path = "../../flux/crates/flux-sys"` and `path = "../libtypio"` assumed a
  specific local directory layout (`/home/.../projects/{flux,typio/libtypio}`)
  that did not exist outside one developer's checkout — `cargo build` from a
  fresh clone failed before reaching the first compile. The deps now point at
  published tags on GitHub:
  - `flux-sys`, `flux-text-sys` → `ming2k/flux-rs` tag `v0.1.0`
  - `libtypio`, `typio-abi` → `typio-ime/libtypio` tag `v0.5.0`
  Cargo.lock pins the exact sha; `cargo publish`/packaging flows now work.
- **`panel.rs` now imports `flux_text_*` symbols from `flux-text-sys`
  (the correct crate) instead of `flux-sys`.** The previous
  `flux_sys::flux_text_measure` / `flux_text_draw` / `flux_text_style` /
  … references never compiled — they referenced symbols that are not in
  libflux (they live in the sibling libflux-text, which has its own bindgen
  crate). Pointer-cast seams added at the three `flux_text_draw` call sites
  and at `text_desc.device` assignment, matching the ABI-identical-pointer
  pattern documented in `flux-text-sys/wrapper.h`. `flux_result_is_ok`
  generalised to `fn <T>(r: T) -> bool` so it accepts either crate's
  `flux_result` enum.

### Added

- **Host-managed candidate selection is now wired (ADR-0012, opt-in).**
  `typio_input_context_commit_candidate`. The pure decision lives in
  `candidate_guard::classify_host_selection` (unit-tested in isolation)
  and the effectful layer in `KeyboardRouter::try_host_selection`.
  Engines that have not opted in are untouched — the Rust port
  deviates from the C ancestor's "intercept arrows by default"
  behaviour to avoid taking over navigation from engines adapted to
  running un-intercepted on this daemon.

### Changed

- **Engine registration switched to libtypio's native Rust API.**
  `App::init` now drives engine discovery through `EngineLoader::load_dir`
  + `TypioInstance::registry_rust_mut` (the mutable Rust accessor added
  to libtypio's Unreleased section), instead of the C ABI
  `typio_registry_register_engine_process` path. EngineLoader is the
  only registration path exercised by the daemon now — capability
  negotiation, language propagation, and `ProcessBackend` construction
  happen in one place. `LoadDirReport::registered` now carries
  `Vec<RegisteredEngine>` (name + type) instead of a bare count so the
  host can keep its "registered N keyboard(s), M voice(s)" log line
  without re-scanning manifests.

- **`app/mod.rs` split further: `app/cli.rs` + `app/signals.rs`.** The
  1004-line daemon file drops to 780 lines by extracting two cohesive
  concerns: CLI parsing (`Cli` + `AppOptions` + the `From<Cli>` impl)
  and signal plumbing (the `SHUTDOWN_FROM_SIGNAL` flag,
  `MODE_CALLBACK_TX` OnceLock, `signal_handler`, `mode_changed_trampoline`,
  and `install_signal_handlers`). `app/mod.rs` now owns lifecycle only.

- **`app.rs` split into `app/` directory (2096 → 1004 lines + 3
  submodules).** The monolithic daemon file is now `app/mod.rs` (App
  struct + init + lifecycle) + `app/event_loop.rs` (the per-tick
  pipeline) + `app/indicator.rs` (banner trigger/render/hide) +
  `app/tray.rs` (registry mutations for tray menu actions). Methods
  stay on `App` via multiple `impl` blocks.

- **`InputMethodState` composition fields extracted into
  `CompositionState`.** The 5 cohesive composition-projection fields
  (`candidates`, `selected_candidate`, `host_managed_selection`,
  `composition_seq`, `pending_commit`) now live on a sub-struct
  accessed as `state.composition.<field>`, reducing the god-struct
  surface and grouping the engine-published state.

- **`PENDING_COMPOSITION` 5-tuple replaced with a named struct.**
  `PendingComposition` (`router.rs`) documents the staged fields by
  name instead of by tuple position.

- **Skip redundant preedit Wayland round-trips on candidate navigation.**
  `drain_composition` now consults the previously-stubbed
  `text_ui_state::text_ui_plan_update` and suppresses the
  `set_preedit_string` + `commit` request when the engine reports the
  same preedit text and cursor as the last composition — the canonical
  Up/Down arrow case, where only the highlighted candidate moves. Per
  arrow press this saves one synchronous Wayland round-trip that the
  compositor had to round through the focused text field.

- **Cache candidate-row measurements across panel flushes.**
  `FluxPanel` now memoises `flux_text_measure` results keyed on the
  candidate strings + render scale; both `ensure_candidate_size`
  (sizing) and `draw_candidates` (per-item placement) consult the
  cache. The Up/Down arrow navigation case — same candidates, only the
  selected index moved — now skips the per-candidate measure FFI loop
  entirely on every redraw.

- **Hot-path `eprintln!` migrated to structured `tracing`.** Grab
  create/destroy, Activate/Deactivate, keymap load/events, panel
  hide/skip-hide decisions, focus-out hide, and the watchdog
  stuck-stage warning now emit through `typio.wayland.*`,
  `typio.panel.host`, and `typio.watchdog` targets, controlled by
  `RUST_LOG` like the rest of the daemon. The unconditional
  stderr-write lock is no longer on the per-keystroke path.

- **Lazy candidate snapshot in the panel-update tick.** The event loop
  now reads only cheap scalars (schedule state, selected, composition
  seq, candidate count) every tick and defers the `Vec<String>` clone
  to the rare flush path, eliminating an allocation per Idle tick.

### Removed

- **C-ABI `register_engine_process` deleted from `app/mod.rs`.** The
  function duplicated `EngineLoader::load_single`'s responsibility
  (manifest → C-ABI `typio_registry_register_engine_process`) but
  existed only because libtypio's `EngineRegistry` was not reachable
  from external Rust callers — `TypioRegistry::inner` is `pub(crate)`.
  With libtypio's new `TypioInstance::registry_rust_mut()` accessor,
  the host now drives registration through the Rust API and the C-ABI
  shim is dead. Its `register_engine_process_round_trip` test was a
  C-ABI smoke test that the EngineLoader integration tests already
  cover more thoroughly.

- **`pw_capture` module deleted (292 lines, never wired).** The
  PipeWire audio-capture port was declared in `lib.rs` but had no
  production callers; only its own unit tests constructed `PwCapture`.
  The `voice` cargo feature, `pipewire` and `libspa` dependencies,
  and `libpipewire-0.3` / `libspa-0.2` system-library requirements are
  gone with it. Voice-engine loading via libtypio's registry still
  works unchanged.

- **Phase 0 spike binary + standalone diagnostic binaries deleted.**
  `src/main.rs` (the package's implicit `typio-host` binary, a "Phase
  0 spike" that only verified Wayland connectivity) and
  `bin/spike-input-method`, `bin/spike-engine-input`,
  `bin/spike-config-watcher`, `bin/check-libtypio`,
  `bin/typio-daemon-stub` (1285 lines total) are removed. Only the
  shipping `typio` binary remains. None were referenced by CI, install
  scripts, integration tests, or docs; their historical context stays
  in the CHANGELOG.

- **`calloop` dependency removed.** Machete-flagged as unused; the
  event loop drives `libc::poll` directly with no `calloop` integration.

- **`panel_coordinator::decide_candidate_flush` deleted.** Despite the
  name it implemented anchor-probe + caret-fallback logic for the
  *positioned-UI* path (the indicator banner), not the candidate panel
  — the candidate panel uses compositor-managed popup positioning and
  the simpler `panel_scheduler::should_flush`. The dead function and
  its test were the last unreconciled stub from an earlier design
  iteration.

- **`text_ui_state::positioned_ui_plan` + `PositionedUiPlan` deleted.**
  Pure decision function superseded by `panel_coordinator`'s
  `Instant`-based inline timeout logic; only its own tests referenced
  it.

### Added

- **Skip redundant preedit Wayland round-trips on candidate navigation.**
  `drain_composition` now consults the previously-stubbed
  `text_ui_state::text_ui_plan_update` and suppresses the
  `set_preedit_string` + `commit` request when the engine reports the
  same preedit text and cursor as the last composition — the canonical
  Up/Down arrow case, where only the highlighted candidate moves. Per
  arrow press this saves one synchronous Wayland round-trip that the
  compositor had to round through the focused text field.

- **Cache candidate-row measurements across panel flushes.**
  `FluxPanel` now memoises `flux_text_measure` results keyed on the
  candidate strings + render scale; both `ensure_candidate_size`
  (sizing) and `draw_candidates` (per-item placement) consult the
  cache. The Up/Down arrow navigation case — same candidates, only the
  selected index moved — now skips the per-candidate measure FFI loop
  entirely on every redraw.

- **Hot-path `eprintln!` migrated to structured `tracing`.** Grab
  create/destroy, Activate/Deactivate, keymap load/events, panel
  hide/skip-hide decisions, focus-out hide, and the watchdog
  stuck-stage warning now emit through `typio.wayland.*`,
  `typio.panel.host`, and `typio.watchdog` targets, controlled by
  `RUST_LOG` like the rest of the daemon. The unconditional
  stderr-write lock is no longer on the per-keystroke path.

- **Lazy candidate snapshot in the panel-update tick.** The event loop
  now reads only cheap scalars (schedule state, selected, composition
  seq, candidate count) every tick and defers the `Vec<String>` clone
  to the rare flush path, eliminating an allocation per Idle tick.

### Added

- **Candidate Panel Behavior explanation doc.** Added
  `docs/explanation/candidate-panel-behavior.md` as the UI-level
  counterpart to `panel-architecture.md` and `frontend-graphics.md`.
  It consolidates the candidate-box lifecycle (hidden → waiting for
  anchor → visible → hidden), the host-managed-selection key
  contract, the anchor generation/probe/caret-fallback path, the
  Retry schedule and watchdog tolerance visible to the user during
  compositor stalls, and the relevant `display.*` config keys — with
  a source-map table pointing at the Rust modules that own each
  responsibility. Wired into `docs/index.md` and cross-linked from
  `panel-architecture.md`.

- **Structured panel diagnostics.** The host now initializes `tracing`
  for the shipping `typio` binary and emits candidate-panel timing as
  structured `typio.panel.timing` events controlled by `RUST_LOG`:
  `info` records slow frames and `trace` records every frame for
  long-session lag investigations. The diagnostic path also adds
  privacy-preserving correlation fields (`composition_seq`, `frame_id`)
  and adjacent targets for queue, engine-key, composition, panel
  scheduler, Wayland I/O, and flux text-cache state.

- **On-screen status indicator (`typio_host::indicator` + `FluxPanel`
  banner).** The transient `<badge> · <engine> · <mode>` popup anchored
  near the caret now actually renders, completing the Wayland-frontend
  phase that ADR-0035 had scoped but left as a stub (`app.rs` carried a
  `TODO: Phase 9` placeholder that discarded the label). Three trigger
  paths are wired, matching ADR-0017/0018: `FirstActivate` (salience +
  3-second acknowledged-recency gates), `Reactivate` (salience only),
  and deliberate change (no gates — covers the Ctrl+Shift chord,
  tray-driven engine/language switches, IPC-driven mutations via
  `typioctl`, and the future `summon_indicator` shortcut). A typed
  `IndicatorConfig` snapshot reads `display.indicator_enabled` and
  `display.indicator_duration_ms` (default 1500 ms, clamped 100–10000)
  once at startup and on reload; the hot path is FFI-free. The
  auto-hide timerfd is armed only when the coordinator actually maps
  the popup, fixing a latent issue where a queued show that later
  flushed through `flush_pending_with_timeout` never armed the timer
  nor updated the recency edge. `FluxPanel::draw_status_banner` and
  `ensure_banner_size` share the candidate panel's Vulkan surface and
  flux text stack per ADR-0017 but use independent layout constants.
  `FocusDriver::tick` now returns the focus transition so the loop can
  layer the indicator on top without re-observing effect state.

- **Tray SNI menu layout + IPC bus wiring (`typio_host::tray_sni`,
  `typio_host::ipc_bus`).** Phase 6 port. The StatusNotifierItem
  `GetLayout` method now serialises the `tray_menu::RegistrySnapshot`
  tree into the dbusmenu `(ia{sv}av)` form, and menu clicks / SNI
  activation gestures are routed through a typed action handler. The
  new `IpcBus` module glues the `UdsServer` to the generic
  `StatusService` using a libtypio-backed `ServiceBackend`; it captures
  `events.subscribe` updates and forwards them to the server. A
  `StateController` listener pushes `engine.changed`,
  `language.changed`, and `runtime.changed` notifications to
  subscribed UDS clients, and updates the tray icon / menu snapshot on
  state changes.

- **Rust input-method session glue (`typio_host::session_glue`).**
  Phase 4 port of the focus-controller pipeline driver. The
  `InputMethodState` now records `activate`/`deactivate`/`done` facts
  for the pure `focus_controller::reduce`/`diff` logic, applies
  surrounding text to the libtypio input context at each `done`, and
  detects the `unavailable` event. The new `FocusDriver` runs once per
  event-loop tick: it derives desired grab/focus state, observes the
  live keyboard grab and keymap-readiness, and applies the minimal
  idempotent effect set in the documented order
  (`discard_composition` → `focus_out` → `destroy_grab` →
  `clear_preedit` → `commit` → `scrub_generation` → `create_grab` →
  `focus_in` → `reactivate`).

  `KeyboardRouter` gained explicit `focus_in`/`focus_out`/`reset`/
  `soft_pause`/`scrub_generation` lifecycle methods so effects can be
  applied at the right time rather than unconditionally at router
  creation. The daemon event loop now calls `ResumeSignal::tick()` each
  iteration and treats a detected suspend/resume gap as a hard boundary
  that forces a full grab teardown and rebuild.

- **Rust candidate panel integration (`typio_host::panel`).** Phase 5
  wiring. The daemon now creates a `FluxPanel` on the existing
  `zwp_input_popup_surface_v2` `wl_surface` during frontend connection,
  marks the panel dirty when the engine publishes a new composition,
  and flushes a redraw via `panel_scheduler::should_flush` in the
  event loop. A transparent `FluxPanel::hide` path clears the popup on
  focus-out / composition discard, and focus-in/reactivate reset the
  cached `text_input_rect` so the next compositor caret update
  repositions the popup. The poll timeout is shortened to the panel
  retry cadence when the scheduler is in `Retry`.

- **Rust keyboard router (`typio_host::keyboard::router`).** Phase 3
  port of the core keyboard-routing pipeline. Creates a libtypio input
  context, routes `zwp_input_method_keyboard_grab_v2` key events to
  `typio_input_context_process_key`, forwards unhandled keys via the
  virtual keyboard, and implements commit/composition callback draining
  so engine output reaches the compositor. Integrates the existing
  `repeat_timer` module so forwarded keys auto-repeat with a default
  delay/interval, and wires the `keyboard_policy` physical-modifier
  tracking into the keysym-to-engine path.

  The new daemon event loop multiplexes the Wayland display fd, the UDS
  IPC fd, and the repeat timer fd in a single `poll()` call.

- **Rust daemon skeleton is now runnable as the primary binary.** The
  new `crates/typio-host/src/app.rs` + `bin/typio.rs` replace the C
  daemon entry point (`src/app.c`, `src/cli.c`, `src/main.c`) for
  development builds. The Rust daemon parses CLI flags (`-c`, `-d`,
  `-E`, `-v`, `--socket`), initializes a `TypioInstance`, loads engines
  from the resolved engine directories, starts the UDS control server,
  connects to the Wayland input-method frontend, and shuts down cleanly
  on `SIGINT`/`SIGTERM`.

  Build-time configuration previously generated by Meson into
  `typio_build_config.h` is now emitted by `crates/typio-host/build.rs`
  and exposed via the `typio_host::build_info` module. Installation of
  the systemd user service, icon theme, and example configs is handled
  by the new `cargo xtask install` command.

  This is an intermediate milestone toward the full C-to-Rust host
  migration; the C daemon in `src/` is still present and can still be
  built via Meson until feature parity is reached.

- **Rust candidate_guard port (`typio_host::candidate_guard`).** Phase 7
  port of the pure parts of `src/wayland/candidate_guard.c` (170 lines
  of C). When an engine publishes a composition with host-managed-
  selection flags set, the host intercepts navigation/selection keys
  before they reach the engine's `process_key`.

  All pure decision logic ported: keysym → [`HostSelKey`] mapping,
  SelKey → [`HostSelCategory`] grouping, [`host_selection_resolve`]
  (target index from current selection + count + key),
  [`host_selection_is_commit`], [`should_consume_key`] (taking the
  two session fields the C version consulted as explicit parameters
  so it works without the session struct).

  12 unit tests covering keysym classification, category grouping,
  commit classification, navigation clamping at list edges, index
  pick out-of-range filtering, the "default intercept arrows when
  candidates exist but no flags are declared" rule, and the flag-gated
  consume decision.

  **Not ported**: `typio_wl_host_selection_try_commit` in C — it needs
  the input context (`session->ctx`) to call `commit_candidate`. The
  pure resolution is in [`host_selection_resolve`]; the actual commit
  is one line at the call site once input-context integration lands.

- **Rust Wayland event loop completion (`typio_host::app`).** Phase 8
  wiring. The Rust event loop now uses the canonical Wayland read
  sequence (`prepare_read_loop` → `poll` → `read_and_dispatch` or cancel)
  and multiplexes the Wayland display fd, UDS IPC fd, repeat timer fd,
  and config watcher fds in one `poll()` call. A `ConfigWatcher` instance
  watches `core.toml`, `platform.toml`, and the `engines/` subdirectory;
  when the debounce timer fires the daemon calls
  `typio_instance_reload_config`, re-syncs the `StateController`, emits a
  `runtime.changed` IPC notification, and refreshes the tray snapshot.

- **Rust Wayland frontend watchdog (`typio_host::watchdog`).** Port of
  `src/wayland/watchdog.c`. A background thread samples loop-stage
  progress at ~1 Hz while armed and kills the process with `SIGKILL` if
  the heartbeat, stage, and stage timestamp stay unchanged in a
  non-restful stage for longer than the stuck threshold. Includes unit
  tests for start/stop, arming/disarming, stall detection, restful-stage
  exemption, and heartbeat prevention.

  Watchdog stage tracking is wired into `run_with_wayland` so each loop
  phase (flush, prepare-read, dispatch-pending, poll, read-events,
  aux-io, panel-update, repeat, config-reload) reports its stage to the
  watchdog and the end-of-tick heartbeat resets to `Idle`.

- **Rust panel coordinator (`typio_host::panel_coordinator`).** Port of
  `src/wayland/panel_coordinator.c`. Tracks the single positioned popup
  surface owner, anchor generation/readiness, caret-rect presence, and
  pending indicator/voice status UI. Implements the anchor probe
  (empty preedit + commit), the anchor timeout deadline, and the caret
  fallback: when a positioned popup times out without a fresh rectangle,
  the coordinator trusts a previously cached caret rect rather than
  dropping the UI.

  The coordinator is owned by `InputMethodState`; `focus_in` and
  `reactivate` reset the anchor generation and send the probe; the
  `text_input_rectangle` event marks the anchor ready; and the event
  loop uses the remaining anchor deadline to bound the `poll()` timeout.

- **Retire C Wayland event loop and watchdog.** Deleted
  `src/wayland/event_loop.c` and `src/wayland/watchdog.c`; the Rust
  event loop and watchdog are now authoritative. Meson compatibility
  stubs (`src/wayland/event_loop_stub.c` and
  `src/wayland/watchdog_stub.c`) keep the legacy C daemon linkable and
  the component-test suite passing while the bilingual migration
  finishes.

- **Cargo test suite + headless daemon integration tests.** Added
  `app.rs` pure-helper tests (CLI parsing, engine registration via the
  C ABI, tray snapshot building, signal flags), `input_method.rs`
  state-helper tests using a test-only frontend constructor that skips
  GPU panel creation, and `tests/typio_daemon.rs` which exercises the
  real `typio` binary headlessly (`--version`, `--help`, `hello`, and
  `daemon.stop`). Documented the cargo workflow in
  `docs/dev/testing.md` and `docs/dev/setup.md`, and added a
  `cargo-test` CI job that builds sibling `libtypio` and `flux` and
  runs `cargo test -p typio-host`.

- **First runnable Rust daemon binary: `typio-daemon-stub`.** Phase 6
  milestone, extended in Phase 7. Wires together the engine_loader +
  uds_server + TIP framing into a minimal but real daemon process.
  typioctl (or any TIP v3 client) can connect to its UDS socket and
  exchange real JSON-RPC frames.

  Methods implemented by the stub:
  - `hello` → real handshake: protocolVersion + daemonVersion + loaded
    engine list
  - `daemon.version` → crate version string
  - `daemon.status` → running flag + socket path + loaded engines
  - `engine.list` → per-engine details (name, display name,
    description, languages, capabilities)
  - `engine.describe` (params: `{"name": "..."}`) → full EngineInfo;
    returns application error code 1 if not loaded, `-32602` if
    `name` param missing
  - `events.subscribe` → wildcard subscription
  - Unknown methods → JSON-RPC `-32601 Method not found` (spec-
    compliant)
  - Known-but-unimplemented methods → `-32603` error with a message
    naming the method, so typioctl shows a useful diagnostic instead
    of a generic "not found"
  - Malformed JSON → `-32600 Invalid Request`

  `--socket PATH` and `--engine-dir PATH` CLI flags override the
  defaults (useful for testing and dev workflows).

  9 integration tests spin up the stub as a child process and verify
  the wire contract via a real `UnixStream` client. Live-verified
  against the real `typio-engine-rime` manifest: returns the full
  EngineInfo (display name "Rime", description, icon name "typio-rime-
  symbolic", languages `["zh"]`, required caps `["preedit",
  "candidates"]`, etc.).

  The daemon does NOT touch Wayland, does NOT route keys, does NOT
  manage a config tree — it's a smoke test for the daemon skeleton,
  proving that the pieces we have ported compose into a runnable
  binary that speaks the real protocol and serves real engine
  metadata.

- **Rust keyboard policy + notifier ports (`typio_host::keyboard_policy`
  + `typio_host::notifier`).** Phase 5 port of the four
  `src/wayland/keyboard/policy/*.c` files (modifiers.c, chords.c,
  repeat_guard.c, tracker.c — 227 lines of C) and
  `src/notify/notifications.{h,c}` (251 lines of C).

  `keyboard_policy` consolidates the four pure-decision files into one
  module: effective-modifier computation, shortcut chord logic,
  repeat gating predicates, per-key tracking state, and keysym
  constants. Introduces the `KeyTrackState` enum (9 variants) and the
  `ShortcutBinding` struct — both used by the eventual keyboard router
  port.

  `notifier` calls the FreeDesktop Notifications D-Bus API via zbus
  (replacing the C version's `sd-bus`/`libsystemd` dependency for
  this codepath). Two-layer API: `send` for immediate delivery,
  `send_coalesced` for per-key rate-limiting via a 16-entry ring
  buffer matching `TYPIO_NOTIFY_RECENT_CAP`.

  21 unit tests: modifier-bit mapping, effective-modifier OR/AND
  logic with owned/unowned generations, repeat cancel transitions,
  chord gating with all/already-triggered/saw-non-modifier cases,
  KeyTrackState name coverage, tracker slice operations, rate-limiter
  ring-buffer eviction + long-key truncation, notification builder
  defaults.

- **Rust UDS server (`typio_host::uds_server`).** Phase 4 port of
  `src/ipc/uds_server.{h,c}` (555 lines of C). Owns a Unix-domain
  listening socket + an internal epoll instance multiplexing all
  accepted client connections. Per-client framing state (length-
  prefixed JSON-RPC), per-client subscription state (for
  `events.subscribe` + server-emitted notifications), and a 1 MiB
  frame-size cap matching the C version.

  The epoll fd is exposed via [`UdsServer::epoll_fd`] for integration
  with any external event loop. The caller installs a request handler
  via `set_handler`; the handler returns a `RequestOutcome` containing
  an optional response and an optional subscription update — this split
  avoids the closure having to call back into `&mut self`.

  7 unit tests including live UDS round-trips: bind, accept, length-
  prefixed framing, handler dispatch, subscription registration via
  `RequestOutcome`, and selective broadcast via `emit`. The C version
  is unchanged and still ships; this is the parallel Rust implementation.

  **Not ported**: `src/ipc/ipc_bus.c` (301 lines of C). That is the
  routing/handler layer that wires UDS requests to TypioInstance +
  TypioStateController. It is heavily coupled to libtypio's C ABI and
  the state-controller machinery — neither of which the Rust host has
  integrated yet. Deferred until enough of the daemon is ported to
  actually serve real method requests.

- **Rust logind resume detector (`typio_host::resume_signal`).** Phase
  3a port of `src/engine/logind/resume.{h,c}` (252 lines of C) and the
  pure decision rules in `src/engine/resume_model.h`. Subscribes to
  logind's `PrepareForSleep` D-Bus signal via `zbus` (no libsystemd
  dependency) on a dedicated worker thread; events are channelled back
  to the caller's thread via `mpsc`. The boottime/monotonic gap
  detector runs per-tick on the caller's side via
  [`ResumeSignal::tick`]. Both detectors deduplicate on a 5-second
  cooldown. 7 unit tests covering the pure gap/cooldown predicates,
  the cooldown dedup behaviour, and the reason-string literals
  (matched against the C version's stable identifiers).

  Introduces `zbus` as the D-Bus library — replacing the C version's
  `sd-bus`/`libsystemd` dependency for this codepath. The same zbus
  pattern will drive the SNI tray port in a later phase.

- **Rust TIP v3 protocol layer (`typio_host::ipc`).** Phase 3b port
  of `src/ipc/tip_protocol.{h,c}` (90 + 38 lines of C) and the JSON
  envelope helpers in `src/ipc/tip_json.{h,c}` (520 lines of
  hand-rolled JSON in C). Replaces the hand-rolled parser/builder with
  `serde_json` and `serde` derives: JSON-RPC 2.0 envelope (Request,
  Response, Notification, Error, Id) round-trips via
  `serde_json::to_string` / `from_str`. Per-method typed `params` /
  `result` structs are intentionally not ported — they will land
  alongside the corresponding handler port (config access, engine
  registry, etc.); the envelope handles untyped `serde_json::Value`s
  meanwhile, exactly what the C version uses.

  19 unit tests: protocol constants match typioctl's wire
  expectations; socket-path resolution under all three env-var
  regimes; round-trip of all three message kinds; standard
  JSON-RPC error codes; a real-world `hello` request/response
  sample verified against typioctl.

- **Rust backoff + keyboard repeat pure-mechanism ports
  (`typio_host::backoff` + `typio_host::repeat_timer`).** Phase 3c
  port of the pure parts of `src/engine/backoff.{h,c}` (43+25 lines)
  and `src/wayland/keyboard/repeat.c`'s timer-arming + modifier-gate
  logic. The keyboard-repeat dispatch (the 130 lines of deep
  xkb_state / focus / candidate-guard coupling in
  `typio_wl_keyboard_dispatch_repeat`) is deliberately NOT ported —
  it needs the keyboard router state machine which hasn't been ported
  yet, and forcing a standalone extraction would produce an awkward
  stub.

  13 unit tests: backoff schedule doubles+clamps correctly,
  should_retry respects the attempt cap, shift-overflow is guarded;
  RepeatTimer starts/stops toggling the armed flag,
  should_repeat_for_modifiers respects Ctrl/Alt/Super suppression,
  interval_from_rate clamps pathological inputs.

- **Rust config_watcher port + calloop spike (`typio_host::config_watcher`
  + `spike-config-watcher` bin).** Phase 2 port of the watch mechanism
  in `src/wayland/runtime_config.c`. Watches the config directory (and
  optionally the engines subdir) via inotify, filters events to
  `core.toml` / `platform.toml`, and debounces reload triggers with a
  one-shot Linux timerfd. Pure mechanism — the frontend side effects
  that the C version mixes in (purge font caches, invalidate panel,
  reload shortcuts, switch voice engine) are intentionally NOT ported;
  the caller receives a typed reload trigger and decides what to do.

  The watcher owns an inotify instance + a timerfd and exposes both raw
  fds for integration with any event loop. The accompanying
  `spike-config-watcher` bin verifies end-to-end: it plugs both fds
  into a real `calloop::EventLoop`, drives the state machine, and
  demonstrates that three burst writes to `core.toml` collapse into a
  single debounced reload (verified live against `/tmp/typio-spike-cfg`).
  Introduces `calloop` as the event-loop foundation for subsequent
  fd-handling subsystem ports (IPC UDS server, PipeWire, sd-bus tray).

  7 unit tests + the live spike. The C version is unchanged and still
  ships; this is the parallel Rust implementation.

- **Rust engine_loader port (`typio_host::engine_loader`).** Phase 1
  port of `src/engine_loader.c` (678 lines of C). Discovers
  `typio-engine-*.toml` manifests on disk, parses them with the `toml`
  crate (the C version rolled its own line-based parser because there is
  no good C TOML library), negotiates host-vs-engine capabilities, and
  registers out-of-process engine backends with libtypio's native Rust
  `EngineRegistry` — bypassing the C ABI entirely. 30 unit tests +
  4 integration tests, the integration tests verifying that every real
  engine manifest shipped by the sibling repos (mozc, rime, sherpa,
  whisper) parses and registers successfully against live libtypio.
  The C version is unchanged and still ships; this is the parallel Rust
  implementation.

- **Rust host crate skeleton (`typio-host`) + Phase 0 wayland spike.**
  Added a cargo workspace at the repo root and `crates/typio-host/` as
  its first member. The spike connects to the live compositor, snapshots
  the global list, and binds `zwp_input_method_manager_v2` and
  `zwp_virtual_keyboard_manager_v1` — proving the cargo + wayland-client
  + wayland-scanner chain works without a hard dependency on the
  `wayland-protocols` crate. Protocol bindings are generated from the
  local XMLs in `protocols/` (same source of truth as the C code's
  wayland-scanner output), with `text-input-unstable-v3.xml` newly
  vendored because input-method-v2's event arg types reference its
  enums. The C daemon is unchanged; meson and cargo coexist during the
  migration.

- **Phase 0.5 libtypio integration spike (`check-libtypio` bin).**
  Added a second bin target that exercises libtypio's native Rust API
  (`core::registry::EngineRegistry`, `core::engine::{EngineType,
  EngineAvailability, EngineError}`) directly — bypassing the C-shaped
  `TypioInstance` / `c_api` layer entirely. This validates the central
  architectural assumption of the migration: the C ABI is purely an
  engine-plugin contract, and the Rust host pays nothing for it. A
  known leak is noted in the spike output — `EngineRegistry::set_instance`
  still takes a `*mut TypioInstance` back-pointer for callbacks, which
  needs replacing with a Rust closure/trait object before the Rust host
  can fully avoid constructing `TypioInstance`.



- **Rust UDS server (`typio_host::uds_server`).** Phase 4 port of
  `src/ipc/uds_server.{h,c}` (555 lines of C). Owns a Unix-domain
  listening socket + an internal epoll instance multiplexing all
  accepted client connections. Per-client framing state (length-
  prefixed JSON-RPC), per-client subscription state (for
  `events.subscribe` + server-emitted notifications), and a 1 MiB
  frame-size cap matching the C version.

  The epoll fd is exposed via [`UdsServer::epoll_fd`] for integration
  with any external event loop. The caller installs a request handler
  via `set_handler`; the handler returns a `RequestOutcome` containing
  an optional response and an optional subscription update — this split
  avoids the closure having to call back into `&mut self`.

  7 unit tests including live UDS round-trips: bind, accept, length-
  prefixed framing, handler dispatch, subscription registration via
  `RequestOutcome`, and selective broadcast via `emit`. The C version
  is unchanged and still ships; this is the parallel Rust implementation.

  **Not ported**: `src/ipc/ipc_bus.c` (301 lines of C). That is the
  routing/handler layer that wires UDS requests to TypioInstance +
  TypioStateController. It is heavily coupled to libtypio's C ABI and
  the state-controller machinery — neither of which the Rust host has
  integrated yet. Deferred until enough of the daemon is ported to
  actually serve real method requests.

- **Rust logind resume detector (`typio_host::resume_signal`).** Phase
  3a port of `src/engine/logind/resume.{h,c}` (252 lines of C) and the
  pure decision rules in `src/engine/resume_model.h`. Subscribes to
  logind's `PrepareForSleep` D-Bus signal via `zbus` (no libsystemd
  dependency) on a dedicated worker thread; events are channelled back
  to the caller's thread via `mpsc`. The boottime/monotonic gap
  detector runs per-tick on the caller's side via
  [`ResumeSignal::tick`]. Both detectors deduplicate on a 5-second
  cooldown. 7 unit tests covering the pure gap/cooldown predicates,
  the cooldown dedup behaviour, and the reason-string literals
  (matched against the C version's stable identifiers).

  Introduces `zbus` as the D-Bus library — replacing the C version's
  `sd-bus`/`libsystemd` dependency for this codepath. The same zbus
  pattern will drive the SNI tray port in a later phase.

- **Rust TIP v3 protocol layer (`typio_host::ipc`).** Phase 3b port
  of `src/ipc/tip_protocol.{h,c}` (90 + 38 lines of C) and the JSON
  envelope helpers in `src/ipc/tip_json.{h,c}` (520 lines of
  hand-rolled JSON in C). Replaces the hand-rolled parser/builder with
  `serde_json` and `serde` derives: JSON-RPC 2.0 envelope (Request,
  Response, Notification, Error, Id) round-trips via
  `serde_json::to_string` / `from_str`. Per-method typed `params` /
  `result` structs are intentionally not ported — they will land
  alongside the corresponding handler port (config access, engine
  registry, etc.); the envelope handles untyped `serde_json::Value`s
  meanwhile, exactly what the C version uses.

  19 unit tests: protocol constants match typioctl's wire
  expectations; socket-path resolution under all three env-var
  regimes; round-trip of all three message kinds; standard
  JSON-RPC error codes; a real-world `hello` request/response
  sample verified against typioctl.

- **Rust backoff + keyboard repeat pure-mechanism ports
  (`typio_host::backoff` + `typio_host::repeat_timer`).** Phase 3c
  port of the pure parts of `src/engine/backoff.{h,c}` (43+25 lines)
  and `src/wayland/keyboard/repeat.c`'s timer-arming + modifier-gate
  logic. The keyboard-repeat dispatch (the 130 lines of deep
  xkb_state / focus / candidate-guard coupling in
  `typio_wl_keyboard_dispatch_repeat`) is deliberately NOT ported —
  it needs the keyboard router state machine which hasn't been ported
  yet, and forcing a standalone extraction would produce an awkward
  stub.

  13 unit tests: backoff schedule doubles+clamps correctly,
  should_retry respects the attempt cap, shift-overflow is guarded;
  RepeatTimer starts/stops toggling the armed flag,
  should_repeat_for_modifiers respects Ctrl/Alt/Super suppression,
  interval_from_rate clamps pathological inputs.

- **Rust config_watcher port + calloop spike (`typio_host::config_watcher`
  + `spike-config-watcher` bin).** Phase 2 port of the watch mechanism
  in `src/wayland/runtime_config.c`. Watches the config directory (and
  optionally the engines subdir) via inotify, filters events to
  `core.toml` / `platform.toml`, and debounces reload triggers with a
  one-shot Linux timerfd. Pure mechanism — the frontend side effects
  that the C version mixes in (purge font caches, invalidate panel,
  reload shortcuts, switch voice engine) are intentionally NOT ported;
  the caller receives a typed reload trigger and decides what to do.

  The watcher owns an inotify instance + a timerfd and exposes both raw
  fds for integration with any event loop. The accompanying
  `spike-config-watcher` bin verifies end-to-end: it plugs both fds
  into a real `calloop::EventLoop`, drives the state machine, and
  demonstrates that three burst writes to `core.toml` collapse into a
  single debounced reload (verified live against `/tmp/typio-spike-cfg`).
  Introduces `calloop` as the event-loop foundation for subsequent
  fd-handling subsystem ports (IPC UDS server, PipeWire, sd-bus tray).

  7 unit tests + the live spike. The C version is unchanged and still
  ships; this is the parallel Rust implementation.

- **Rust engine_loader port (`typio_host::engine_loader`).** Phase 1
  port of `src/engine_loader.c` (678 lines of C). Discovers
  `typio-engine-*.toml` manifests on disk, parses them with the `toml`
  crate (the C version rolled its own line-based parser because there is
  no good C TOML library), negotiates host-vs-engine capabilities, and
  registers out-of-process engine backends with libtypio's native Rust
  `EngineRegistry` — bypassing the C ABI entirely. 30 unit tests +
  4 integration tests, the integration tests verifying that every real
  engine manifest shipped by the sibling repos (mozc, rime, sherpa,
  whisper) parses and registers successfully against live libtypio.
  The C version is unchanged and still ships; this is the parallel Rust
  implementation.

- **Rust host crate skeleton (`typio-host`) + Phase 0 wayland spike.**
  Added a cargo workspace at the repo root and `crates/typio-host/` as
  its first member. The spike connects to the live compositor, snapshots
  the global list, and binds `zwp_input_method_manager_v2` and
  `zwp_virtual_keyboard_manager_v1` — proving the cargo + wayland-client
  + wayland-scanner chain works without a hard dependency on the
  `wayland-protocols` crate. Protocol bindings are generated from the
  local XMLs in `protocols/` (same source of truth as the C code's
  wayland-scanner output), with `text-input-unstable-v3.xml` newly
  vendored because input-method-v2's event arg types reference its
  enums. The C daemon is unchanged; meson and cargo coexist during the
  migration.

- **Phase 0.5 libtypio integration spike (`check-libtypio` bin).**
  Added a second bin target that exercises libtypio's native Rust API
  (`core::registry::EngineRegistry`, `core::engine::{EngineType,
  EngineAvailability, EngineError}`) directly — bypassing the C-shaped
  `TypioInstance` / `c_api` layer entirely. This validates the central
  architectural assumption of the migration: the C ABI is purely an
  engine-plugin contract, and the Rust host pays nothing for it. A
  known leak is noted in the spike output — `EngineRegistry::set_instance`
  still takes a `*mut TypioInstance` back-pointer for callbacks, which
  needs replacing with a Rust closure/trait object before the Rust host
  can fully avoid constructing `TypioInstance`.

### Removed

- **Legacy C host and Meson scaffold.** The root `src/`, root `tests/`,
  Meson project files, generated-header template, and subproject wraps are
  removed. The Rust `typio-host` crate is now the only typio-linux host
  build, test, and install surface; `cargo xtask install` handles package
  staging and installation.

### Fixed

- **Preedit caret always rendered at the right edge.** libtypio already
  publishes the caret byte offset in `TypioComposition.cursor_pos`, but
  the host's `on_composition` callback dropped it on the floor and
  `drain_composition` hardcoded `cursor = preedit.len()` when calling
  `set_preedit_string`. As a result left/right navigation inside a
  composition (e.g. rime's edit mode) never moved the visible caret.
  `PENDING_COMPOSITION` now carries the raw `cursor_pos` from the ABI,
  and `drain_composition` resolves it through the same
  `preedit::resolve_cursor` rule used by `build_plain_preedit`
  (non-negative preserves the value, negative falls back to end-of-text)
  so the compositor finally sees the engine's intended caret position.
  The `typio.engine.composition` trace now also records `cursor_pos`
  for future investigations.

- **Candidate panel froze and the daemon was SIGKILLed by the
  watchdog after prolonged rime paging — four root causes, all
  fixed.** The diagnostic trace evolved across four iterations:

  1. **Per-glyph GPU submit (flux `atlas.c` + `layout.c`).** Every
     glyph cache miss fired a full `vkQueueSubmit2` +
     `vkWaitForFences` cycle inside `flux_vk_upload_to_image` — one
     GPU pipeline stall per new glyph, 40+ stalls per frame for
     Chinese candidates. Fixed by deferring uploads: cache-miss
     blits expand a dirty bounding box via `txt_atlas_mark_dirty`;
     `txt_atlas_flush` batch-uploads the box in a single
     `flux_image_update_region` before each `draw_glyph_run`.

  2. **Atlas full-reset death spiral (flux `atlas.c`).** The 2048×2048
     atlas held ~4200 CJK glyphs at HiDPI scale and was constantly
     full. Every new glyph triggered `atlas_reset`, which re-rasterised
     **all** 4200 cached entries via FreeType (40-55 ms each, 10
     resets per frame = ~470 ms). Fixed by replacing `atlas_reset`
     with `atlas_clear`: O(1) cursor reset + cache invalidation, no
     re-rasterisation. Atlas also enlarged to 4096×4096 (16 MB).

  3. **`vkQueuePresentKHR` blocked the main loop for 15+ s (flux
     `surface.c` + `frame.c`).** Even after the text path was fast,
     the synchronous present call stalled under compositor
     back-pressure, blocking the single-threaded event loop. Fixed
     with a dedicated **present thread**: `flux_frame_present` now
     rotates frame state and enqueues a present request (ring
     buffer + binary-semaphore pool of 4), returning immediately. The
     present thread calls `vkQueuePresentKHR` without holding
     `queue_lock` (Mesa's WSI uses its own internal lock), so the
     main thread's `vkQueueSubmit2` proceeds concurrently. When the
     pool is exhausted (compositor severely behind), frames are
     submitted but not presented — dropped, not blocked.

  4. **Watchdog Present-stage threshold (typio-linux `watchdog.rs`).
     `LoopStage::Present` was added so the watchdog attributes the
     time correctly; with the async present thread it is
     non-restful (the enqueue itself is fast — a stall there is a
     genuine bug). A `before_present` callback lets the caller
     transition the stage before the (now-instant) present call.

- **Watchdog still SIGKILLed the daemon during rapid rime candidate
  paging.** The earlier heartbeat-between-calls fix (see the entry
  below) covered every FFI call *except* the one that actually
  blocks: `vkQueuePresentKHR` inside `flux_frame_present`. Under
  compositor back-pressure the WSI dispatches Wayland events
  synchronously inside the present, and with the swapchain images
  held by the compositor a single present call stalled for >3 s —
  long enough to trip the watchdog's default threshold with no way
  to heartbeat from the same thread. The trace pattern was
  diagnostic: every sub-step (`begin_frame`, `canvas_begin`,
  `text_draw_loop`, `canvas_end`, `frame_submit`) completed in
  <0.2 ms, then the process died inside `frame_present` before its
  timing line was printed. A new `LoopStage::Present` is now set
  immediately before `flux_frame_present` in both
  `FluxPanel::draw_candidates` and `FluxPanel::draw_status_banner`
  (via a `before_present` callback the callers in `App` fill with
  `wd.set_stage(LoopStage::Present)`). The watchdog gained a
  per-stage stuck threshold: `Present` tolerates 15 s
  (`PRESENT_STUCK_MS`) vs the 3 s default, absorbing transient
  present stalls while still catching a genuine deadlock eventually.
  `stuck_threshold_ms()` on `LoopStage` drives the detection so
  future stages can tune their own thresholds.

- **Occasional stuck key after release (the "stuck backspace"
  symptom).** `InputMethodState.pending_key` was a single
  `Option<DecodedKeyEvent>` slot, overwritten by every `Event::Key`
  callback. When the Wayland library delivered two key events in the
  same dispatch batch — most commonly a backspace release immediately
  followed by another key event — the second event overwrote the
  release in the slot, and the event-loop driver (which drained only
  one event per iteration via `take_pending_key()`) never saw the
  release. The repeat timer, armed on the press, kept firing
  `RepeatOutcome::Consumed` for the consumed key forever, long after
  the user had physically released the key. The slot is now a
  `Vec<DecodedKeyEvent>` queue (`pending_keys`) drained in arrival
  order via `take_pending_keys()`; the loop processes every queued
  event per iteration so releases always reach `router.on_release` +
  `timer.stop()`. The regression test in
  `input_method::tests::state_helpers_round_trip` now pushes a press
  and a release and asserts the drain preserves order. The same
  drain pattern was applied to the `spike-engine-input` dev binary.

- **Rapid candidate cycling tripped the PanelUpdate watchdog.** When
  the user held the next-candidate key, the engine emitted a
  composition callback per repeat, each scheduling a panel flush.
  Once the compositor fell behind releasing swapchain images — a
  single `vkQueuePresentKHR` or `vkAcquireNextImageKHR` call inside
  `FluxPanel::draw_candidates` blocked for >3 s — the daemon was
  SIGKILLed by the watchdog, which had no heartbeat inside the
  render call and so could not distinguish "slow but progressing"
  from "hung." `draw_candidates` and `draw_status_banner` now take a
  `&dyn Fn()` heartbeat callback and invoke it between every
  blocking FFI call (begin_frame / canvas_begin / text loop /
  canvas_end / frame_submit / frame_present). `App::run_with_wayland`
  and `App::render_indicator_banner` pass `&|| wd.heartbeat()` and
  also heartbeat between `set_scale`, `ensure_*_size`, and the draw
  call. A genuinely deadlocked FFI call still stops heartbeating and
  the watchdog still fires; only legitimate slow renders are now
  tolerated. The heartbeat is extracted as `wd_ref` before the
  mutable `frontend` borrow so the closure capture is disjoint from
  the panel's `&mut self`. Per-stage timing logs (`draw_candidates:
  begin_frame took X ms`, …) mirror the existing
  `draw_status_banner` instrumentation so any future stall can be
  pinned to the exact FFI call from the stderr trace.

- **First indicator banner killed the daemon at HiDPI scales.** The
  `FluxPanel` swapchain was pre-allocated at 256×128 physical pixels
  to keep the first automatic indicator banner off the
  `flux_surface_resize` path (which does `vkDeviceWaitIdle` + WSI
  swapchain release and trips the 3 s `LoopStage::PanelUpdate`
  watchdog on a fresh daemon). The sizing comment in
  `InputMethodFrontend::connect` only audited the **height** axis —
  and at scale 2 the default Rime indicator label `中 · Rime · 懿拼音`
  needs 280 px physical wide → 320 px after the 64 px grow-only
  quantum, exceeding the 256 px allocation and forcing the very
  resize the pre-allocation was meant to avoid. The panel was
  SIGKILLed on every fresh daemon start under a scale-2 display. The
  pre-allocation is now exposed as `PANEL_PREALLOC_WIDTH` /
  `PANEL_PREALLOC_HEIGHT` (512×128) with the width/height audit
  table for scales 1, 1.5, 2 and 3 documented on the constants, so
  future tuning has both dimensions to reason about instead of
  repeating the height-only oversight. The previous commit 0a91080
  (height quantisation) and this width audit together close the
  first-banner watchdog class.

- **Keyboard shortcuts swallowed during soft-pause.** When the
  keyboard grab was retained across a `deactivate` (soft pause) but no
  text field was active (`state.active == false`), key events from the
  grab were silently dropped — never queued for the engine and never
  forwarded to the virtual keyboard. This made application shortcuts
  (Ctrl+S, Alt+Tab-like chords handled by apps, etc.) stop working
  after defocusing a text field while the daemon held the grab. The
  grab's `Key` event handler now forwards keys directly to the virtual
  keyboard when the text input is not active, so unhandled keys reach
  the focused application throughout the grab's lifetime.

- **Indicator behaviour description contradicted ADR-0018.**
  `docs/explanation/wayland-input-method.md` claimed the indicator
  "stays hidden on `REACTIVATE`", but ADR-0018 explicitly states the
  indicator "re-reveals correctly on reactivation" and the C
  implementation (`focus_effects.c`) actually re-evaluates against the
  salience gate on `Reactivate`. The doc table is updated to reflect
  the three-path model (focus / reactivate / deliberate-change) with
  the correct gate semantics, and the surrounding paragraph no longer
  contradicts the design record.

- **Tray badge rendering was clipped, off-centre, and visually muddy.**
  `icon_badge::render_one` used a fixed `px = size*0.82` font scale and
  `baseline = size*0.78` heuristic tuned for Latin glyphs. CJK glyphs
  (中 / あ) routinely overshoot the font's em box, so the heuristic
  pushed them past the bottom of the 16/22/24px pixmaps and the lower
  strokes were silently cropped. The 8-pass diagonal dark halo then
  collapsed the counter-form (the middle of "中" filled in), leaving
  an unreadable blob. The rasteriser now measures the combined pixel
  bbox of the shaped glyph run, picks the largest scale factor whose
  bbox fits inside the canvas minus a 1px outline halo on every side,
  and shift-centres the bbox both horizontally and vertically. The
  halo uses 8-direction 1px offsets for sizes >= 24 (where counter-
  forms stay open) and a 4-direction 1px-plus-2px-thickened halo
  below 24 (so fine strokes stay distinct). Net effect: 中 / EN / あ
  render completely, centred, and legible at every SNI pixmap size.

- **Tray icon never showed the active language badge at startup.** The
  daemon boot path activated only the first keyboard engine
  (`typio_registry_set_active_keyboard`) and never called
  `typio_registry_restore_language`, so the registry's `active_language`
  stayed `None` even when rime declared `zh` and the user's last session
  was in Chinese. With no active language, `resolve_language_icon`
  fell through to layer 3 and the tray rendered
  `typio-keyboard-symbolic` instead of the expected `中` badge. The
  daemon now calls `typio_registry_restore_language` after engine
  discovery (re-activating the last-used language if still enabled,
  otherwise the first enabled language), and only falls back to the
  first registered keyboard when no languages are declared at all.

- **IPC-driven state changes did not refresh the tray icon.** Mutations
  issued through the UDS control surface (`typioctl language use en`,
  `keyboard.next`, `voice.use`, `config.reload`, `engine.load`, …)
  mutated the registry directly via the C ABI but never signalled the
  daemon core, so the `StateController` snapshot, the tray badge, and
  the tooltip kept showing the previous language/engine until something
  else (a tray menu click, a config-watcher reload) happened to fire a
  `StateRefresh`. `StatusService` now exposes a `state_change_callback`
  wired through `IpcBus::set_state_change_callback`; the daemon core
  installs a callback that pushes `DaemonEvent::StateRefresh`, so the
  main loop re-syncs the controller and tray surfaces on every
  successful mutation. Read-only methods and errored mutations do not
  fire the callback.

- **Auto-repeat for engine-consumed keys (Backspace, etc.).** When an
  active engine consumed a key — e.g. a pinyin engine handling Backspace
  to delete the last preedit character — the repeat timer was explicitly
  stopped and there was no path for repeats to re-enter the engine, so a
  long press only ever produced one keystroke. The router now records
  the consumed key in `Engine` repeat mode, arms the timer the same way
  the forwarded-key path does, and re-dispatches each repeat tick with
  `is_repeat: true` (previously hardcoded `false`) so the engine can
  keep deleting/composing one step per interval. The forwarded-key path
  keeps its existing `Forward` mode behaviour.

- **Compositor repeat-info was ignored.** The grab's `RepeatInfo` event
  was surfaced as a lifecycle callback that no daemon subscriber ever
  consumed, so the host always fell back to the X-server defaults
  (600 ms delay / 30 Hz). The compositor-provided `(rate, delay)` is now
  stored on `InputMethodState` and consulted when arming the timer via
  the new `repeat_timer::resolve_repeat_params` helper. A compositor
  rate of `0` (the protocol signal for "do not repeat") now correctly
  suppresses auto-repeat entirely.

- **Candidate popup layout and clearing.** The flux-backed candidate
  popup now centers text with measured glyph metrics, detaches the
  Wayland buffer when hidden so stale black popup shadows do not remain
  beside the caret, and renders smaller muted number labels before each
  candidate.

- **Virtual-keyboard protocol error 0 (`no_keymap`) on first forwarded
  key.** The grab's `Keymap` event loaded the compositor keymap into
  local xkbcommon state but never forwarded it to the
  `zwp_virtual_keyboard_v1`. As soon as the engine declined a key and
  the host called `forward_key`, the compositor rejected the request
  with `error 0: 'key' sent before keymap` and tore down the
  connection. The grab handler now dups the keymap fd and pushes a
  matching `vk.keymap` request before any `key`/`modifiers` requests
  can be issued, so the unified grab + vk-keymap resource described in
  ADR-0003 actually reaches `Ready`.

- **Runaway auto-repeat after a single key press.** The grab's `Key`
  event handler only queued press events into `pending_key`, so the
  event-loop driver's release branch — which calls `timer.stop()` and
  forwards the release to the focused app — was dead code. The repeat
  timer therefore stayed armed forever and re-sent the last forwarded
  key on every interval. Release events are now queued alongside
  presses, so the timer is stopped and releases reach the virtual
  keyboard as expected.

- **Modifier shortcuts (Ctrl-C, Ctrl-V, …) arrived as bare keys.**
  The grab's `Modifiers` event updated local xkb state but never called
  `vk.modifiers`, so the virtual keyboard always reported an empty
  modifier mask. Forwarded keys reached the focused app without their
  Ctrl/Alt/Shift context. The handler now mirrors the grab's modifier
  mask to the vk on every `Modifiers` event (vk keymap is guaranteed to
  be set by then since the grab sends `Keymap` first).

- **Composition preedit was discarded, leaving only candidates.**
  `on_composition` built the preedit string from `TypioComposition`
  segments but stored only `(candidates, selected)` in
  `PENDING_COMPOSITION` — the preedit was thrown on the floor. To
  compensate, `drain_composition` then used `candidates.first()` as
  the preedit, which produced no inline preedit whenever the engine
  emitted preedit without candidates yet (the common pinyin case
  after a single keystroke). The slot now stores
  `(preedit, candidates, selected)` and `drain_composition` flushes
  the real preedit to the compositor while candidates drive the
  popup independently.

- **Second segfault on Ctrl+C shutdown.** The first drop-order fix
  (`panel` before `state`) was necessary but not sufficient:
  `InputMethodFrontend.conn` (the Wayland `Connection`) was still
  declared before `panel`, so the display socket was closed before
  libflux's teardown finished. The full reorder is now `panel` →
  `state` → `queue` → `conn`, so libflux never touches a closed
  connection.

- **Duplicate engine registration warning from `.installed.toml`
  shadow files.** The Meson build of typio-engine-rime generates both
  `typio-engine-rime.toml` (source paths) and
  `typio-engine-rime.installed.toml` (install paths) in the same
  `build/` directory; the loader loaded both and tripped
  `TypioErrorAlreadyExists`. `is_manifest_filename` now excludes the
  `.installed.toml` variant.

- **Watchdog killed the daemon while cycling rime candidates (ADR-0013
  port).** The Rust `FluxPanel` sized its swapchain to the exact
  candidate-row width and called `flux_surface_resize` on every width
  change. Each resize runs an unbounded `vkDeviceWaitIdle` + swapchain
  rebuild + WSI compositor roundtrips on the single-threaded IME loop;
  under rapid candidate paging (e.g. holding Down) one roundtrip
  exceeded the watchdog's 3 s `PanelUpdate` threshold and the daemon
  was SIGKILLed. The panel now binds `wp_viewporter`, allocates the
  buffer quantised to 64 px grow-only, and crops to the exact content
  rect via `wp_viewport.set_source` / `set_destination`. After a short
  warm-up `flux_surface_resize` is not called again during steady-state
  paging. Compositors without `wp_viewporter` fall back to the
  previous exact-size resize.

### Added

- **Ctrl+Shift engine-switch chord.** The pure
  `chord_should_switch_engine` predicate existed in
  `keyboard_policy` but was never called from the main loop, so the
  standard Linux IME switch shortcut did nothing. `KeyboardRouter`
  now tracks the gesture (`saw_non_modifier`, `already_triggered`)
  and exposes `take_switch_chord_fired`, which `App::run` drains
  after every keypress: when the chord fires it cycles the active
  keyboard (via new `cycle_active_keyboard` helper) and sends
  `DaemonEvent::StateRefresh` so the tray icon and menu update the
  same way they do for a tray-click switch. The default binding is
  Ctrl+Shift (any side, any order).

- **Tray slot never appeared in waybar / KDE / GNOME shell.**
  `Tray::register` passed `org.kde.StatusNotifierItem-{pid}-1` to the
  watcher without first requesting that well-known name on the session
  bus, so the watcher could not resolve the service to our connection
  and no slot appeared. `register` now calls `request_name` before
  `RegisterStatusNotifierItem`.

- **Segfault on Ctrl+C shutdown.** Two independent drop-order bugs:
  (1) `KeyboardRouter::drop` calls `typio_input_context_free` on a
  pointer that `TypioInstance::drop` already freed, because
  `App::shutdown` dropped the instance first and left the router
  dangling. (2) Inside `InputMethodFrontend`, `state` (which owns the
  popup `wl_surface`) was dropped before `panel` (which holds a raw
  pointer to that surface for libflux teardown). Both fixed:
  `App::shutdown` now drops `router` / `frontend` / `state_controller`
  before the instance, and `InputMethodFrontend`'s struct fields were
  reordered so `panel` drops first.

### Changed

- **Daemon event channel replaces ad-hoc `AtomicBool` flags.** Cross-thread
  requests to the main loop (IPC `daemon.stop`, tray menu actions) now flow
  through a single typed `mpsc::Receiver<DaemonEvent>` drained once per
  tick, replacing the previous `SHUTDOWN_REQUESTED` / `RESTART_REQUESTED`
  / `STATE_DIRTY` flag trio. The SIGINT/SIGTERM handler still uses an
  `AtomicBool` (`SHUTDOWN_FROM_SIGNAL`) because `mpsc::Sender::send` is
  not async-signal-safe. As a side effect, switching engine / language /
  voice from the tray menu now correctly refreshes the tray icon,
  tooltip, menu snapshot, and IPC `runtime.changed` subscribers — the
  previous code mutated libtypio directly and never propagated the
  change back to the Rust `StateController`. The exec-restart path in
  `App::finish` now reads `self.saw_restart` (set during drain) instead
  of polling `RESTART_REQUESTED`.

- **Build commands in contributor docs no longer wrap every command in
  a `( cd … && … )` subshell.** `README.md`, `CONTRIBUTING.md`,
  `docs/dev/setup.md`, `docs/dev/testing.md`, and
  `docs/how-to/package-for-distribution.md` now run from the
  `typio-linux` repo root using `cargo --manifest-path ../libtypio/…`
  and `meson compile -C ../../flux/build`. The setup layout diagram
  also now shows `flux` as a sibling of `typio/` (its real location),
  not as a child of it.

- **Wayland input-method source maps in the explanation docs now point
  at Rust files.** `docs/explanation/wayland-input-method.md`,
  `focus-controller.md`, and `input-method-session.md` previously
  listed deleted C paths (`src/wayland/*`, `src/engine/*`, `src/ui/*`).
  The tables now name the `crates/typio-host/src/*.rs` modules that
  own each responsibility after the bilingual migration (ADR-0035).

- **Engine setup examples in the docs now reference the real sibling
  engine repos.** `typio-engine-basic` was renamed to
  `typio-engine-compose` (commit `22fcca1` in that repo) but
  `README.md`, `docs/dev/setup.md`, `docs/how-to/troubleshooting.md`,
  and `docs/how-to/package-for-distribution.md` still told contributors
  to build it. The docs now point at `typio-engine-compose` (Cargo,
  manifest in repo root), `typio-engine-rime` (Meson, manifest in
  `build/`), and `typio-engine-mozc` (Meson, manifest in `build/`),
  with a table mapping each engine to its build system, manifest
  path, and language coverage.

## [0.3.4] - 2026-06-20

### Fixed

- **`-Werror=switch` failure in `ipc_bus.c`.** `TYPIO_STATE_CHANGE_LANGUAGES`
  (added in ADR-0034 alongside the dynamic engine capabilities work) was
  handled by the tray's switch but not by the IPC bus's, so
  `-Dwerror=true` (CI's build job) rejected the build. The case is now
  present with a no-op body documenting the gap: IPC clients that cache
  `language.list` results will be stale until a future `languages.changed`
  topic is added (separate, protocol-bumping work).

- **`-Werror=sign-conversion` failures in `test_menu_model.c`.** Three
  call sites passed `typio_tray_menu_item_get_child_count()` (returns
  `size_t`) straight into `check_int` (takes `long`). Added a
  `check_size` helper mirroring the existing `check_int` / `check_bool`
  pair rather than scattering casts.

### Changed

- **flux dependency pinned to v0.1.0** (first standalone release). Bumped
  `subprojects/flux.wrap` from the dangling `v0.0.8` pin — that tag never
  existed in the flux repo, so the wrap was silently failing and the build
  was resolving to whatever `flux.pc` pkg-config found first. Added an
  explicit `version: '>= 0.1.0'` floor on the `dependency('flux')` lookup
  so a stale system install can no longer mask a broken wrap.

- **flux diagnostics now flow through `typio_log`.** The device log
  callback registered with `flux_device_create` was a no-op that swallowed
  every `FLUX_LOG_*` record, including pipeline-creation warnings,
  `frames_in_flight` clamps, and `flux_canvas_save` overflow reports. It
  now maps the five flux log levels onto the matching `TypioLogLevel`
  values and emits `flux: <msg> (<file>:<line>)` records, so GPU-side
  diagnostics show up in `journalctl --user -u typio` next to the host's
  own logs.

### Fixed

- **Sibling-flux symlink resolved to the wrong path.** The meson helper
  that auto-links `../flux` into `subprojects/flux` for dev checkouts
  computed both the sibling-existence probe and the link target with one
  `..` too few — it looked for `<typio-meta-repo>/flux` instead of
  `<projects>/flux`. The bug was masked because the broken subproject
  lookup fell through to pkg-config and accepted any installed `flux`.
  The new `>= 0.1.0` version floor made the fallthrough reject the stale
  system 0.0.8 and surfaced the symlink bug; both the probe and the link
  target are now computed with the correct nesting depth.

### Removed

- **Dead `src/ui/panel/stub.c` no-op fallback.** The CHANGELOG entry in
  v0.3.x made flux a hard requirement of the Wayland build, but the
  meson `else` branch wiring `stub.c` was left in place. With the new
  version floor the unreachable branch is now an explicit `error()`
  instead of silently compiling a panel that returns `NULL` from every
  entry point. `docs/explanation/frontend-graphics.md` updated to drop
  the stale claim that the stub "proves the upper pipeline compiles
  against an empty backend"; the CPU-only tests under `tests/ui/` are
  the actual proof.

## [0.3.3] - 2026-06-20

### Fixed

- **Candidate-selection lag after long typing sessions.** Four independent
  contributors compounded into progressively worse panel latency over hours
  of mixed-scale CJK typing:

  - **`font_cache` was unbounded and O(N) per lookup.** Every unique
    `(font_path, size, weight)` tuple — multiplied by fractional-scale
    jitter (1.0 / 1.25 / 1.5 / 1.75 / 2.0 …), output hot-plug events, and
    per-codepoint CJK fallback expansion — appended a permanent entry to a
    linearly-scanned array, lengthening the per-keystroke lookup that the
    layout LRU performs on every candidate navigation. The table is now a
    bounded open-addressing hash (`FONT_OBJ_CACHE_CAP = 256`) with LRU
    eviction, so lookup stays O(1) regardless of session length. The
    underlying `FT_Face` (the part that mmaps ~5–17 MB per file) is kept
    alive in a separate face table for the process lifetime, since
    `TypioTextShape` borrows it; only the per-tuple wrapper (`hb_font_t` +
    path string) is evicted.
  - **Fontconfig's internal caches grew monotonically.** Every
    `FcFontSort` cache miss inflated Fontconfig's process-global state
    without bound; `FcFini()` was only called on explicit config reload.
    The font resolver now drains Fontconfig every 256 codepoint misses
    (`FONTCONFIG_PURGE_PERIOD`), a cadence aligned with the per-codepoint
    fallback memo's working set so it fires roughly once per "the memo has
    fully churned" rather than on every lookup.
  - **Glyph atlas reclaim contract mismatch.** The header documented
    "rebuild on 75% load OR packer exhaustion" but the implementation only
    honoured packer exhaustion, so a long session with many small glyphs
    that filled the hash table without saturating the texture never
    reclaimed — probe chains lengthened toward O(n) per glyph. Both
    triggers now fire as documented.
  - **Each cache miss was its own `vkQueueSubmit` + `vkWaitForFences`.**
    After an atlas reclaim emptied the texture, the next frame
    re-rasterised every visible glyph (tens for a 10-row CJK panel) as
    separate GPU round-trips, producing a multi-millisecond hitch on the
    first navigation after each reclaim. Misses within a frame now
    coalesce into a single submit via the new `glyph_atlas_flush()` /
    `glyph_upload_regions()` path, called from `do_present` after the
    canvas is recorded and before the render pass is submitted.

- **Two candidate-snapshot memory leaks.** `typio_wl_session_destroy`
  freed the surrounding `TypioWlSession` struct without clearing the
  `candidate_snapshot` embedded by value in it, leaking the heap-owned
  `candidates` array plus 3×N strings on every reconnect / session
  recreate. The `discard_composition` focus effect reset the engine and
  hid the panel but never cleared the snapshot, leaking the same per
  focus-out / engine-switch. Both paths now route through the shared
  `typio_wl_session_clear_candidate_state()` helper (mirroring
  `on_commit_callback`).

### Added

- **`font_cache` LRU + hash test suite** (`tests/ui/test_font_cache.c`).
  Exercises the cap enforcement, FT_Face sharing across (size, weight)
  variants, stable pointer identity for cache hits, face survival across
  LRU eviction (the use-after-free guard), and clear/reset behaviour.
  Skips with meson exit code 77 when the pinned Inter font path is
  unavailable so the suite still builds on minimal toolchains.

- **Candidate-snapshot lifecycle test suite**
  (`tests/wayland/test_candidate_snapshot.c`). Exercises the
  `typio_wl_session_clear_candidate_state` / `typio_candidate_snapshot_clear`
  paths now shared by `session_destroy`, `discard_composition`,
  `on_commit_callback`, and `on_composition_callback`. Verifies heap strings
  are freed, the helper is idempotent (no double-free across the four call
  sites that can fire in sequence on a focus-out → commit → destroy
  transition), and the candidate-guard scalars are zeroed.

- **Diagnostics for the font / atlas / Fontconfig layer.**
  `TypioTextShaperDiag` and the slow-render `text_shaper_log_diag` trace
  now report: atlas batched-flush count + peak batch size + total regions
  flushed; current FontObj table occupancy (`font_obj_count`/`cap`) + face
  count + cumulative LRU evictions; cumulative Fontconfig purge count.
  These let a slow-render log distinguish steady-state warm-atlas operation
  from a post-reclaim re-warm storm, and a font_cache thrash from a
  Fontconfig cost spike — directly correlating the panel lag with the
  cache layer responsible.

### Changed

- **Snapshot helpers extracted to `src/wayland/candidate_snapshot.{c,h}`.**
  Previously static inside `input_method.c`, the snapshot clear / assign /
  equal helpers now live in their own TU so the free path can be
  unit-tested without linking the Wayland protocol surface. Behaviour is
  unchanged; only location and visibility moved.

## [0.3.2] - 2026-06-19

### Fixed

- **Tray icon blurriness.** The SNI `IconPixmap` channel previously
  shipped only `{22, 44}` px rasters; common tray hosts (Waybar,
  Swaybar, GNOME AppIndicator, KDE Plasma) request 16/22/24/32 px and
  were forced to scale, producing a blurry badge especially on HiDPI /
  fractional-scaled outputs. The ladder is now
  `{16, 22, 24, 32, 44, 48, 64, 96, 128}` so the host can pick a close
  fit at any DPI.
- **Tray badge legibility with a voice engine configured.** The corner
  microphone overlay (`OverlayIconName`) was composited on top of the
  rendered language badge, turning 3-character badges (`Рус`, `الد`)
  into an unreadable blob at typical tray sizes. The overlay is now
  suppressed while a badge is active; the voice presence is still
  advertised in the tooltip and the menu. Badge ⇄ icon transitions
  also emit `NewOverlayIcon` so the host re-queries the corner overlay.

### Added

- **`summon_indicator` shortcut** (default `Ctrl+Super+i`). Actively
  re-shows the on-screen indicator (language · engine · mode) on
  demand, instead of only on focus/engine-change triggers. The
  indicator uses `zwp_input_popup_surface_v2`, so the shortcut only
  fires while a text field is focused — the coordinator drops the
  request silently otherwise. Requires libtypio ≥ 0.4.2.

## [0.3.1] - 2026-06-19

### Fixed

- README no longer mentions the D-Bus status interface (removed in
  ADR-0008) and no longer references the nonexistent `-Denable_status_bus`
  meson option; the systray is correctly described as sd-bus / libsystemd.
- `docs/dev/setup.md` lists `libsystemd` (sd-bus) instead of `dbus-1` as
  the systray prerequisite; dropped `enable_status_bus` from the meson
  options table.
- `docs/reference/stability.md` now reports the current `protocolVersion`
  as `3` (was `2`) and includes `language.*` in the TIP method-surface row.

## [0.3.0] - 2026-06-13

### Added

- **Language-first switching**
  ([ADR-0031](docs/adr/0031-language-first-switching-surface.md), requires
  libtypio >= 0.4). Ctrl+Shift now cycles the enabled language list and
  retargets the keyboard and voice slots together; installs without language
  metadata fall back to keyboard-engine cycling. Engine manifests gain a
  `languages` array key. TIP bumps to protocol v3 with the
  `language.{list,use,next,prev}` methods, `daemon.status.activeLanguage`,
  and the `language.changed` event. Languages with no keyboard engine are
  layout-only: keys pass through raw (e.g. Moroccan Darija on an Arabic
  layout).
- `CONTRIBUTING.md`, `SECURITY.md`, an interface stability reference
  (`docs/reference/stability.md`), and a security-model explanation
  (`docs/explanation/security-model.md`).
- `-Denable_fuzzers=true` builds a libFuzzer harness for the TIP JSON
  parser (`tests/fuzz/fuzz_tip_json.c`, requires clang).

### Fixed

- Configure no longer fails on meson < 1.4 (Ubuntu 24.04 LTS): the C
  standard is spelled `c2x`, which every supported meson and compiler
  accepts. This was breaking every CI run at the configure step.
- `subprojects/libtypio.wrap` pointed at the repository's pre-rename
  location and a moving `main` revision; it now fetches
  `typio-ime/libtypio` pinned to `v0.3.0`.

### Changed

- The build now requires libtypio >= 0.4.0 (pkg-config version floor in
  `meson.build`; the wrap and CI pin follow), the first release with the
  language-first registry API.
- CI builds against a pinned libtypio release tag and adds two jobs: an
  ASan/UBSan test run and a non-blocking canary against libtypio `main`.
  The primary job builds with `-Dwerror=true`.
- Engine manifests now use `protocol = "typio-engine-protocol"` and register
  with libtypio through `typio_registry_register_engine_process`. Engine traffic
  uses the private fd 3 Typio Engine Protocol channel; stdout and stderr are
  reserved for logs.

## [0.2.0] - 2026-06-06

### Changed

- **Engine discovery is now manifest based.** The daemon scans
  `typio-engine-*.toml` manifests, registers engines through
  `typio_registry_register_engine_process`, and starts engine processes for
  engine calls. The daemon no longer loads engine `.so` files in-process.
  Engine packages ship direct engine executables.

- **Engine package paths now match the engine process model.** The system manifest
  directory moved to `<datadir>/typio/engines`, while engine
  executables belong under `<libexecdir>/typio/engines`. Installed manifests
  should point `command` at the absolute engine executable path.

## [0.1.17] - 2026-06-05

### Changed

- **Renamed the project `typio-wayland` → `typio-linux`.** The old name
  framed the display protocol as the whole project and capped its scope;
  this host is the Linux home for Typio, with Wayland as its current (and
  only) frontend. Updated the meson project name, the `--version` output,
  the README (now "Typio for Linux — a Wayland-native input method host",
  noting X11 is not supported and not planned), and every doc reference.
  The installed binary name (`typio`) is unchanged.

- **Renamed the platform config file `wayland.toml` → `platform.toml`.**
  *Breaking, no backward-compat fallback:* rename
  `~/.config/typio/wayland.toml` to `platform.toml`. Paired with the
  platform-independent `core.toml`, the two filenames now self-document
  the original split — portable core config vs host/platform-specific
  config — instead of naming the file after one display protocol.

- **Renamed `src/voice/` → `src/audio/`.** The directory only holds the
  PipeWire audio-capture layer (implementing `TypioAudioSource`); voice
  recognition lives in engine plugins. The PipeWire node
  `typio-voice-capture` is now `typio-audio-capture`. The voice-input
  feature vocabulary (`enable_voice`, `TypioVoiceSession`, …) is
  deliberately unchanged — it names the feature, not the capture layer.

- **All Wayland host code consolidated under `src/wayland/`.**
  `src/frontend/` was a role word that dropped the one defining trait —
  Wayland — already carried by every symbol (`TypioWlFrontend`,
  `TYPIO_WL_FRONTEND_H`), and Wayland handling was split across
  `src/frontend/` and `src/input/wayland/`. `src/input/` had also
  collapsed to a single `policy/` child whose "platform-agnostic"
  framing did not hold: those files are all `typio_wl_*`, consume xkb
  modifier masks, and reach into `TypioWlFrontend`. The moves:
  `src/frontend/` → `src/wayland/`; `src/input/wayland/` →
  `src/wayland/keyboard/` (with `keyboard.c`); `src/input/policy/` →
  `src/wayland/keyboard/policy/`; the now-empty `src/input/` is gone.
  The pure-vs-effectful split survives as `wayland/keyboard/` (I/O
  mechanics) over `wayland/keyboard/policy/` (pure, unit-tested logic).
  Tests mirror the layout under `tests/wayland/`. All `#include`
  directives, `src/meson.build` + `tests/meson.build` source lists and
  `include_directories`, and the `docs/` cross-references were updated.
  Pure relocation (`git mv` + include-path edits); no behaviour change.
  Build is clean and all 18 host tests pass.

### Fixed

- **The SNI tray never registered after the sd-bus migration.** The
  migration registered hand-written vtables for the reserved
  `org.freedesktop.DBus.Properties` and `…Introspectable` interfaces;
  `sd_bus_add_object_vtable` rejects those with `-EINVAL`, and the
  error path destroyed the tray and returned `NULL` from
  `typio_tray_new`, so no `StatusNotifierItem` was ever registered and
  the icon never appeared. Properties are now `SD_BUS_PROPERTY` rows
  with a single `sd_bus_property_get_t` getter; `Properties.Get`/`GetAll`
  and the Introspectable interface are synthesised by sd-bus. The
  hand-written introspection XML and the invalid
  `sd_bus_message_open_container(m, 'v', NULL)` variant code are gone.
  The DBusMenu `GetLayout` / `GetGroupProperties` / `AboutToShow`
  handlers appended their reply to the sealed incoming message
  (`-EPERM`); they now build a proper reply via
  `sd_bus_message_new_method_return`. Verified live against the
  quickshell `StatusNotifierWatcher`: the item registers, `GetAll`
  returns the icon, and the menu layout renders.

## [0.1.16] - 2026-06-05

### Changed

- **`src/platform/monotonic.h` → `src/clock.h`.** The single-header
  `src/platform/` directory was removed; the header now lives at the
  top level of `src/` (reachable via the existing `.` include path).
  Updated all 14 consumer `#include` lines and the cross-reference in
  `docs/adr/0015-candidate-popup-lag-final-fixes.md`.

### Removed

- **`typio_dump_recent_log` and the legacy on-disk ring-buffer dump.**
  Five fatal-exit call sites (VK broken state in `bridge.c`, emergency
  exit in `router.c`, two in `keyboard.c`, watchdog timeout in
  `watchdog.c`) now log the cause and either stop or `SIGKILL` directly.
  `typio_app_finish` no longer dumps the buffer before a normal
  shutdown. The `recent_log_dump_path` field on `TypioApp` and the
  `src/recent_log.h` header are gone. `app.c` no longer needs
  `<dirent.h>`. systemd's journald already captures stderr for
  post-mortem analysis, and the legacy `typio-recent-*.log` file
  sweep that ran at startup is dropped.

### Changed

- **All D-Bus clients migrated from libdbus to sd-bus (libsystemd).**
  The three libdbus surfaces — logind's `PrepareForSleep` subscriber
  in `engine/logind/resume.c`, the desktop-notifications client in
  `notify/notifications.c` (currently not built), and the SNI tray
  host in `tray/{sni,bus}.c` + `tray_internal.h` — now use
  `sd-bus` from `libsystemd`. The SNI tray was the largest change:
  `DBusObjectPathVTable.message_function` (one vtable per path) became
  one `sd_bus_vtable` per `(path, interface)` pair registered with
  `sd_bus_add_object_vtable`; signal subscription went from
  `dbus_bus_add_match` + a global filter to `sd_bus_match_signal`
  returning a per-subscription `sd_bus_slot`; the shared
  `dbus_helpers.h` `a{sv}` dict-entry builders were inlined into
  `sni.c` as four `append_dict_*` helpers. The dependency is
  `dependency('libsystemd')` in `meson.build`; the build macro is
  `HAVE_LIBSYSTEMD` (renamed from `HAVE_LIBDBUS`).

### Removed

- **`src/dbus_helpers.h`.** All callers (the SNI tray only, after
  migration) have inlined the four helpers they need.

- **`-ldbus-1` dependency.** The host now links only `libsystemd` for
  D-Bus access; libdbus is gone from the link line.

## [0.1.15] - 2026-06-04

### Fixed

- **Shortcuts stopped working after committing on Enter.** A focus-out that
  retains the keyboard grab (soft pause, to skip the expensive rebuild on
  re-focus) only zeroed the xkb modifier mask, leaving `physical_modifiers`,
  `saw_blocking_modifier`, and the shortcut arbiter untouched. A modifier held
  at defocus stayed phantom-held — its release is dropped by the routing guard —
  and corrupted the Ctrl+Shift engine-switch chord detection on the next
  activation. `typio_wl_keyboard_pause()` now also scrubs that host-side
  arbitration state.

### Removed

- **`--list` / `-l` CLI option.** Engine inspection now lives in the separate
  `typioctl` client, which queries a running daemon over its UDS socket. The
  in-process `typio_app_list_engines` path and its flag have been removed.

## [0.1.14] - 2026-06-04

### Changed

- **Install `typio` into PATH.** The daemon binary now installs to
  `<prefix>/<bindir>/typio`; the systemd user unit points at that path.
- **Production engine discovery excludes the user lib directory by default.**
  `typio_engine_dirs_build()` now returns CLI override, `TYPIO_ENGINE_DIR`, and
  the compile-time system directory only. Development and test engines must be
  enabled explicitly with `--engine-dir` or `TYPIO_ENGINE_DIR`.

## [0.1.13] - 2026-06-05

### Fixed

- **ABI version validation in plugin loader.** `plugin_loader.c` now resolves
  `typio_engine_abi_version` from each shared object and calls
  `typio_engine_abi_check()` before making any vtable calls. Plugins with
  mismatched ABI are rejected with a clear log message, preventing SIGSEGV
  from struct layout divergence at runtime.

### Changed

- **Engine discovery order: system before user.** `typio_engine_dirs_build()`
  now returns the system directory (`$PREFIX/$LIBDIR/typio/engines`) before the
  user directory (`~/.local/lib/typio/engines`). The full priority order is:
  CLI override → `TYPIO_ENGINE_DIR` env var → system directory → user
  directory. This ensures production engines take precedence over
  development/test builds in the user's home directory.

## [0.1.12] - 2026-06-05

### Changed

- **Deferred engine availability query at init.** `typio_wl_frontend_new` now
  defaults `keyboard_availability` to `TYPIO_ENGINE_PREPARING` instead of
  eagerly calling `typio_registry_get_active_keyboard_availability`. This
  prevents the daemon from crashing if a third-party engine plugin is buggy
  during startup. The push-based availability callback transitions to
  `TYPIO_ENGINE_READY` when the engine finishes warm-up.

## [0.1.11] — 2026-06-04

### Added

- **FT_Face sharing across (size, weight) tuples.** `font_cache.c` now
  maintains a shared face table: each unique font file is mmap'd once
  (one `FT_New_Face` per file). Distinct (size, weight) tuples create
  separate `FontObj` entries with their own `hb_font` and `font_id`, but
  reference the shared `FT_Face`. `font_cache_apply()` sets the face's
  pixel size and variable-font weight before each shaping or rasterisation
  call. Reduces memory usage by ~85 MB for CJK fonts (one 17 MB mmap
  instead of six).

### Changed

- **Input-first event loop scheduling.** Panel flush now runs at the end
  of each iteration, after all input events have been dispatched. If GPU
  work stalls (atlas reclaim, fence timeout), the next iteration still
  processes queued input before attempting another panel render.
- **Glyph upload fence timeout.** `glyph_upload.c` now uses a 100 ms
  timeout for `vkWaitForFences` instead of `UINT64_MAX`. If the GPU is
  stalled (driver hang, memory pressure), the glyph is skipped rather
  than freezing the event loop indefinitely.
- **Atlas reclaim only on packer exhaustion.** `glyph_atlas_reclaim` now
  triggers only when the shelf packer is exhausted (texture full), not
  when the hash table reaches 75% load. With 131072 slots and ~3000
  unique CJK glyphs, the table is well below 75% even after hours of
  use; packer exhaustion is the real signal that the texture needs
  rebuilding.
- **Keyboard grab reuse across focus changes.** `transition_to_inactive`
  now calls `typio_wl_keyboard_pause` (soft reset: release forwarded keys,
  disarm repeat, reset XKB modifier state) instead of destroying the
  keyboard grab. `transition_to_active` reuses the existing grab if it
  survives the deactivate. Eliminates the `xkb_keymap_new_from_string`
  compile (~5–20 ms), the Wayland `grab_keyboard` roundtrip, and the
  `NEEDS_KEYMAP` window that dropped keys between focus events. The full
  rebuild path is retained for resume-from-suspend, watchdog recovery,
  and reconnect paths.

## [0.1.10] — 2026-06-03

### Fixed

- Clear host-side candidate guard state when a commit ends composition, so
  Left/Right after committing no longer resurrect stale candidate lists.
- Gate key routing on `TypioEngineAvailability`: while the active keyboard
  engine is preparing, key presses and releases are consumed instead of being
  forwarded to the application or latched into repeat.

## [0.1.9] — 2026-06-03

### Fixed

- **Candidate Panel scheduling no longer mixes dirty updates with present
  retries.** Replaced the old pending boolean with an explicit `IDLE` /
  `DIRTY` / `RETRY` schedule state. Candidate navigation now only marks the
  latest snapshot dirty; rendering and protocol commits run from the event-loop
  Panel stage, and the 16 ms retry cadence applies only to a focused,
  flushable present retry. (ADR-0023)

## [0.1.8] — 2026-06-03

### Fixed

- **Candidate Panel could stay stale after a present RETRY.** Removed the
  persistent `present_retry` latch that let the event loop skip every future
  Panel flush after one stalled present. Panel updates now return `OK` /
  `RETRY` / `FAIL`, and retry scheduling is driven by the current update
  result rather than durable surface state. (ADR-0022; scheduling later
  formalized by ADR-0023)

### Changed

- **Daemon startup is systemd-user-only.** Removed installed `.desktop`
  launch/autostart entries; packagers should enable `typio.service` so the
  daemon has one supervised process, restart policy, duplicate-start
  protection, and journal log stream. (ADR-0021)

## [0.1.7] — 2026-06-02

### Fixed

- **Candidate panel went blank/stale after extended CJK input.** The glyph atlas
  shelf packer only ever advanced and the old hash-only compaction explicitly
  abandoned texture space, so once the 2048² image saturated (a few thousand
  distinct glyphs — routine for a CJK IME) new glyphs never packed again and
  rendered blank permanently. ADR-0019's root cause was inverted: the texture
  fills long before the hash table reaches its 75 % threshold, so compaction
  rarely ran and never reclaimed the binding resource. Replaced it with
  `glyph_atlas_reclaim()`, a wholesale atlas rebuild (texture + hash table +
  packer + counts) triggered on 75 % hash load **or** packer exhaustion; the
  next draw re-rasterises the visible page lazily. (ADR-0020, supersedes ADR-0019)
- **Uncached fallback-font resolution.** The per-codepoint `FcFontSort` (over
  every installed font) ran on every layout re-creation under LRU churn; the
  coverage-keyed cache built to prevent this was never wired in. Added a
  per-`(codepoint, weight)` resolution memo and removed the dead
  `fallback_cache` module. (ADR-0020)

### Changed

- **Glyph/font layer modularized.** Split the 1600-line `text_shaper.c` (~380
  now) into `glyph_upload`, `glyph_atlas`, `font_cache`, and `font_resolve`,
  each header documenting its Bound/Evict/Reclaim/Observe contract. (ADR-0020)

### Added

- **Glyph-layer diagnostics.** `typio_text_shaper_log_diag()` /
  `typio_text_shaper_get_diag()` expose atlas fill, shelf height, cumulative
  rebuilds, glyphs rasterised, and fallback memo hit/miss; wired into the panel
  slow-render path so a stall logs glyph-layer state inline. (ADR-0020)

## [0.1.6] — 2026-06-02

### Fixed

- **Panel UI lag after extended CJK input sessions.** The glyph atlas hash table
  accumulated dead entries as LRU-evicted layouts' glyphs were never removed,
  degrading lookup from O(1) to O(n) via linear-probe chains. Added automatic
  hash-table compaction (triggered at 75 % load) that rebuilds the table with
  only live entries — pure CPU work (~100 μs), no GPU involvement.
  `GLYPH_SLOT_CAP` increased 4× (32 K → 128 K) to delay first compaction.
  (ADR-0019)

## [0.1.5] — 2026-06-02

### Changed

- **Qualified ADR-0012 references as libtypio ADR.** The CHANGELOG and source
  code comments now prefix "ADR-0012" with "libtypio" to avoid confusion with
  typio-linux's own ADR-0012 (shared glyph atlas). Fixed digit key range
  from "1–9" to "0–9" in historical entries.

## [0.1.4] — 2026-06-02

### Added

- **`INDEX_0` host-managed selection key.** Digit `0` now selects the 10th
  candidate (index 9). Added `TYPIO_WL_HOST_SEL_COMMIT_INDEX_0` enum value
  and corresponding keysym mapping, resolve logic, and commit detection in
  `candidate_guard.c`.

## [0.1.3] — 2026-06-02

### Added

- **`COMMIT_RAW` host-managed selection action (libtypio ADR-0013).** Enter/KP_Enter
  is now classified separately from Space. When the engine sets the
  `TYPIO_HOST_SEL_COMMIT_RAW` flag, the host commits the raw preedit text
  instead of the selected candidate. `router.c` gains raw-commit logic using
  `typio_wl_build_plain_preedit` + `typio_input_context_commit`.

## [0.1.2] — 2026-06-02

### Fixed

- **Host-managed candidate selection keys now actually work.** `router.c`
  previously intercepted navigation/commit/index-pick keys (consuming them
  so they never reached the engine or the application) but forgot to act on
  them. Added `key_route_handle_host_selection` which updates the local
  selected index and re-renders the panel for arrow keys, and calls
  `typio_wl_host_selection_try_commit` for Space/Enter/digit keys.

## [0.1.1] — 2026-06-02

### Changed

- **`candidate_guard` now respects per-capability flags (libtypio ADR-0013).**
  `typio_wl_candidate_guard_should_consume` classifies each keysym into
  Navigate / Commit / IndexPick categories and checks the corresponding
  bit in `session->last_host_managed_selection` instead of the old coarse
  `bool`. This lets engines retain control over digits and space while
  still delegating arrow-key navigation and enter/space commit to the host.
- `last_host_managed_selection` field on `TypioWlSession` widened from
  `bool` to `uint32_t`.

## [0.1.0] — 2026-06-02

### Added

- **Host-managed candidate selection (libtypio ADR-0012).** `candidate_guard.c`
  intercepts Up/Down/Left/Right, digit keys 0–9, Space, and Enter when
  `host_managed_selection = true`. The host maintains the selected index
  and commits via `typio_input_context_commit_candidate`.
- **Profile fields in IPC payload.** `engine.statusChanged` now includes
  `profileId` and `profileLabel` alongside mode fields.

### Removed

- **Engagement-based routing.** The host no longer bypasses the engine
  based on `engagement`. `key_route_should_forward_basic_text` and
  `TRACK_BASIC_PASSTHROUGH` are deleted; all keys flow to the active
  engine's `process_key`.
- **Old status API references.** All surfaces updated from
  `TypioKeyboardEngineStatus` to `TypioKeyboardEngineMode`.

### Changed

- **IPC payload shape.** `engine.statusChanged` removes `engagement`;
  adds `profileId` and `profileLabel`.
- **Tray tooltip and indicator logic.** No longer engagement-aware;
  derives display purely from mode metadata.

## [0.0.9] — 2026-06-01

### Added

- Add `engine.load`, `engine.unload`, and `engine.reload` IPC methods for
  runtime engine hot-reload. `engine.reload` accepts an optional `path` param
  for explicit-path loading (development workflow) or rescans engine_dirs when
  omitted (production). New plugin_loader API: `typio_plugin_load_single()`,
  `typio_plugin_unload()`, `typio_plugin_reload()`.

### Fixed

- Fix stale preedit after engine switch via Ctrl+Shift. The arbiter now clears
  the old engine's composition, the compositor-facing preedit, and the candidate
  panel before switching engines. A safety net in `typio_on_engine_change` ensures
  the same cleanup runs for any engine switch path (tray menu, IPC, etc.),
  preventing underlined text from lingering when the new engine does not recognize
  the previous composition state.

## [0.0.8] — 2026-06-01

### Fixed
- Make Panel UI ownership explicit so indicator auto-hide can no longer hide candidate UI after typing starts. Candidate, indicator, and voice status requests now arbitrate through one positioned UI owner and one anchor-readiness model, with a default anchor probe for browser cursor placement. (ADR-0017)
- Fix CJK and symbol rendering in the candidate panel. The text shaper now performs per-glyph font fallback via `FT_Get_Char_Index` when the primary font produces .notdef glyphs, resolving each missing codepoint against Fontconfig-sorted candidates. Up to 4 fallback fonts per text run are cached and reused. (ADR-0016)
- Fix supplementary-plane characters (emoji, rare CJK) rendering as tofu. Font loading now selects a format-12 charmap when available, enabling correct lookup of codepoints above U+FFFF. (ADR-0016)
- Fix primary font resolving to Latin-only variant when a CJK variant of the same family exists. `match_font_file` now verifies CJK coverage and retries with a charset constraint if needed. (ADR-0016)

### Added
- Add project glossary (`docs/reference/glossary.md`) with canonical term definitions and vocabulary replacement table.
- Add writing conventions, required docs inventory, review process, ADR workflow, and cross-reference rules to the documentation style guide.
- Rename `panel-ontology.md` to `panel-architecture.md` and consolidate duplicated term definitions into the glossary.

## [0.0.7] — 2026-05-31

### Added
- Add voice push-to-talk (PTT) support via PipeWire capture and sherpa-onnx engine integration.
- Add `voice_ptt` shortcut (default Super+V) and voice session lifecycle (recording → inference → commit).
- Add `typio-engine-sherpa` plugin option to meson for building the sherpa-onnx voice engine.
- Add runtime config reload for voice engine changes without restart.
- Add `engines.sherpa-onnx.model` config key to `core.toml.example` with upstream model directory name.

### Fixed
- Fix voice session double-free: result text was freed by both libtypio `fire_event` and host callback.
- Fix `typio_free_string` used instead of `free()` for Rust-allocated voice result text in host callback.
- Fix tray status icon logic to avoid carrying stale dynamic icons across engine switches.
- Fix plugin loader to correctly handle engine discovery paths.
- Fix controller state handling for voice PTT key tracking.

### Changed
- Refactor app initialization to streamline engine loading and voice session setup.
- Rename meson build options for consistency.
- Unify and refresh configuration, setup, and troubleshooting documentation.
- Update `core.toml.example` with voice engine and sherpa-onnx model configuration.

## [0.0.6] — 2026-05-30

### Added
- Support two-axis engine-status ABI (`active` + `enabled`) and expose `delete_surrounding` capability to plugins.
- Restore per-app profile directory for isolated state when running multiple instances.

### Fixed
- Fix panel font-cache use-after-free that caused CJK glyphs to blank over time.
- Fix tray status icon logic to avoid carrying stale dynamic icons across engine switches.

### Changed
- Rename all internal `typiod` identifiers to `typio` (types, functions, header guards, build variables).
- Refresh developer documentation for engine discovery, memory budgets, and panel vocabulary.

## [0.0.5] — 2026-05-30

### Fixed

- **Key releases now reach the engine (Rime schema switching on Shift,
  etc.).** The keyboard router only ever forwarded key presses to
  `typio_input_context_process_key`; release events were hard-stopped
  in both the main loop (`app.rs` only dispatched when `key.state == 1`)
  and the router (`dispatch_key` early-returned for releases), and the
  ABI event type was hardcoded to `TypioEventKeyPress`. Engines that
  detect a lone-modifier gesture on release — most notably Rime's
  `ascii_composer.switch_key.Shift_L/Shift_R` used for schema/ASCII
  toggling — could never complete the gesture. The router now forwards
  releases with `TypioEventKeyRelease`, and the main loop follows the
  same consume/forward contract as presses (drain on consume, otherwise
  forward to the virtual keyboard). The Ctrl+Shift engine-switch chord
  already suppresses its modifier presses from the engine; the matching
  releases are now suppressed symmetrically via a new
  `engine_tracked_mods` bitfield so the engine never observes an
  unpaired release.


## [0.0.4] — 2026-05-29

### Fixed

- **First indicator banner no longer trips the watchdog at daemon
  startup.** `ensure_banner_size` and `ensure_candidate_size` shared
  an asymmetric grow-only policy: the swapchain width was quantised
  to 64 px and never shrunk (ADR-0013), but the height was exact-
  matched against `phys_height`. The pre-allocated 256×64 swapchain
  from `InputMethodFrontend::connect` covered a 40 px banner on the
  width axis (cropped via `wp_viewport`) but triggered a height
  resize from 64 → 40 px on the very first banner. `flux_surface_resize`
  does `vkDeviceWaitIdle` + swapchain recreate, which blocks on the
  compositor's swapchain-image release; on a fresh daemon the
  compositor has nothing to release yet, and the wait pushed the
  panel-flush stage past the 3 s watchdog, killing the process.
  Height is now quantised to 32 px and grow-only, matching the width
  path; the shared resize+viewport-crop logic lives in a single
  `FluxPanel::apply_grow_only_size` helper so the two callers cannot
  drift apart again.


## [0.0.3] — 2026-05-29

### Fixed
- Make `flux` a required dependency when Wayland is enabled, preventing silent fallback to a no-op popup stub.
- Explicitly link `libvulkan` to resolve `vkCreateWaylandSurfaceKHR` undefined reference at link time.

## [0.0.2] — 2026-05-29

### Fixed
- Use `get_option('sysconfdir')` instead of hard-coded `prefix / 'etc'` for autostart desktop file install path.

## [0.0.1] — 2026-05-29

### Added
- Initial project structure.
