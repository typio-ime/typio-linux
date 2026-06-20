# ADR-0035: Bilingual migration of the host to Rust

- **Status**: Accepted
- **Date**: 2026-06-21
- **Deciders**: Project maintainers

## Context

`typio-linux` is ~25,000 lines of C23 against a tightly-coupled set of
Linux platform APIs (Wayland client, xkbcommon, Vulkan via flux,
PipeWire, sd-bus, inotify/timerfd/poll, UDS). Debugging ergonomics
in C are the chronic pain point: the state machines
(`keyboard/arbiter`, `panel_scheduler`, `candidate_guard`,
`focus_controller`, `watchdog`) encode hard-won compositor-quirk
workarounds that are difficult to verify safe under refactoring.

Three facts about the surrounding codebase make this C host an
anomaly:

1. **`libtypio` is already Rust.** The host today calls into it
   through a C ABI (`typio/abi/`, `typio/runtime/`, `extern "C"`
   `typio_*` entry points, raw-pointer `TypioInstance`,
   `CString`-marhsalled returns). That ABI exists *purely* to bridge
   the language gap with the host.
2. **`flux` already has Rust FFI bindings** (`flux-sys`). The
   Panel/glyph stack calls into it through those bindings today; a
   Rust host would use the same path unchanged.
3. **The Wayland ecosystem in Rust is production-grade.** Smithay's
   `wayland-client` + `wayland-scanner` are used by cosmic, helix,
   alacritty, sway-related tools, etc. `zbus` is a complete
   `libsystemd`-free D-Bus replacement; `calloop` is purpose-built
   for the wayland+timerfd+inotify+UDS event loop this daemon runs.

The question is not whether the Rust ecosystem can support the host
(it can), but whether the architectural payoff justifies the cost of
porting ~25k LoC of carefully-debugged C, and how to structure the
port so it does not destabilise the shipping v0.3.x daemon or the
engine plugin contract.

## Decision

Migrate the host to Rust through a **bilingual coexistence** with
five structural commitments. These are the commitments; the per-file
porting order is operational detail tracked in CHANGELOG entries.

### D1. libtypio stays an independent repository; typio-linux consumes it via path dep

`libtypio` is the framework library below the host in the dependency
stack. It is consumed independently by five external engines
(mozc, rime, sherpa, whisper, hello-template), `typio-settings`,
and — through its `typio-engine-abi.pc` pkg-config file — anything
that links an engine plugin. Folding it into `typio-linux` would be
a layering inversion: a downstream consumer (the host) cannot absorb
an upstream library (the framework) without dragging every engine
into a host-version lockstep.

`typio-linux` adds a cargo workspace at its repo root with
`crates/typio-host/` as the first member. The workspace declares
`libtypio = { path = "../libtypio" }` so dev builds resolve against
the sibling checkout; production builds keep resolving via
`PKG_CONFIG_PATH` or the published crate. The `meson.build` `wrap`
mechanism for libtypio is unchanged during the migration; engines
are untouched.

This mirrors the existing pattern used for `flux` — `flux` is a
sibling checkout symlinked from `subprojects/flux`. The pattern is
established and works.

### D2. Drop the `wayland-protocols` crate; generate all protocol bindings from local XMLs

typio-linux already ships five protocol XMLs in `protocols/`:
`input-method-unstable-v2`, `virtual-keyboard-unstable-v1`,
`fractional-scale-v1`, `viewporter`, `ext-foreign-toplevel-list-v1`.
The C code generates `*-client-protocol.h` and `*-protocol.c` from
them via the C `wayland-scanner` tool.

The Rust port uses `wayland-scanner` (the Rust crate) against the
*same* XMLs. This deliberately rejects the `wayland-protocols`
crate, even though it offers ready-made bindings for some of these
protocols. Reasons:

- Two of the five protocols typio needs (`input-method-v2`,
  `virtual-keyboard-v1`) are not shipped in `wayland-protocols`
  0.32.13 anyway. The crate only includes `input-method-v1`.
- Splitting the protocol set — some from a crate, some from local
  XMLs — produces two codegen paths and an inconsistent story for
  which XML is authoritative for which interface.
- The C code's invariant "the XMLs in `protocols/` are the single
  source of truth for typio's wire protocols" is preserved exactly.

A sixth XML — `text-input-unstable-v3.xml` — is newly vendored into
`protocols/`. `input-method-v2.xml` references its
`change_cause` / `content_hint` / `content_purpose` enums in event
arg types; the C `wayland-scanner` treats these as documentation
annotations, but the Rust `wayland-scanner` emits strongly-typed
references that must resolve. The host never binds a
`zwp_text_input_manager_v3` global — the file is purely a codegen
dependency.

`bitflags` and `wayland-backend` are added as direct dependencies of
`typio-host`. The code emitted by `wayland-scanner` references those
crates by name; making them direct deps is the same pattern
`wayland-protocols` itself follows.

### D3. The Rust host consumes libtypio via `core::*`, not via the C ABI

`libtypio` has two parallel surfaces. The C ABI (`c_api/`, the
`TypioInstance` struct in `instance.rs`, the `extern "C"` `typio_*`
entry points scattered through `instance/`, `input_context/`,
`config/`, etc.) is a translation layer over a clean native Rust
API in `core/` (`EngineRegistry`, the `Engine` / `KeyboardEngine` /
`VoiceEngine` trait hierarchy, `Result<T, EngineError>`,
`EngineMode`, `EngineCapabilities`).

The Rust host bypasses the C ABI entirely and drives `core::*`
directly. The C ABI's role contracts to **engine-plugin contract
only** — the surface that the C/C++ worker binaries (mozc, rime,
sherpa, whisper) link against. Host consumption stops being a
reason to expand or even maintain `c_api/`.

A known leak: `EngineRegistry::set_instance(&mut self, raw: *mut
TypioInstance)` takes a C-shaped back-pointer for callbacks. This
predates the migration and is the one place the C-ABI shape bleeds
into the supposedly native `core::*` API. A follow-up in `libtypio`
replaces this with a Rust trait or closure so the Rust host never
has to construct `TypioInstance`.

### D4. Bilingual coexistence: meson and cargo both build during the migration

`typio-linux` keeps its `meson.build` and the existing C source
tree intact and shippable throughout the migration. The cargo
workspace at the repo root grows new Rust subsystems in parallel;
the C code they replace is deleted only after the Rust replacement
is verified (builds, passes tests, runs end-to-end against a live
compositor).

Subsystems are ported in roughly leaf-to-root order so that each
Rust port can be exercised in isolation before being wired into the
next layer up. Rough phase ordering (operational detail, not part
of this ADR's contract):

1. **Spikes** (Phase 0 / 0.5, done) — verify cargo+wayland chain
   and libtypio `core::*` accessibility.
2. **Leaf subsystems** — `engine_loader`, IPC `uds_server` + TIP
   JSON, `runtime_config`. Pure I/O + state, no Wayland, no UI.
3. **Keyboard state machines** — `router`, `arbiter`, `tracker`,
   `repeat`, `watchdog`. Highest-value targets for Rust enums and
   `Result<T, E>`; also the highest-risk because they encode the
   compositor-quirk workarounds.
4. **Wayland frontend** — `frontend`, `input_method`, `indicator`,
   `panel_coordinator`, foreign-toplevel identity.
5. **Panel/glyph/font stack** — still calls flux via FFI; no
   change to flux itself.
6. **SNI tray** — `zbus` replaces `sd-bus`; libsystemd dependency
   can be dropped.
7. **PipeWire voice** — optional, lowest priority.

When the last C file is deleted, `meson.build` is reduced to flux
integration only, or retired entirely if flux gains a cargo-native
build by then.

### D5. No monorepo

The meta-workspace at `/home/ming/projects/typio/` (typio-linux,
libtypio, flux, engines, typioctl, typio-settings, typio-docs)
remains a sibling-checkout workspace, not a single git repo. Each
component keeps its own version, CHANGELOG, release cadence, and
contributor surface.

Cross-repo atomic commits are achieved via path dependencies in
cargo (`libtypio = { path = "../libtypio" }`) and meson
(`subprojects/flux` symlink), both of which are existing patterns.
No git submodules, no monorepo merge.

## Alternatives considered

- **Big-bang rewrite.** Rejected: 25k LoC of carefully-debugged C
  state machines, 6.4k LoC of C tests, and 34 ADRs of accumulated
  behaviour cannot be re-derived correctly in one pass. A big-bang
  fork would either freeze v0.3.x features for months or produce
  two divergent codepaths. Bilingual coexistence keeps the shipping
  daemon shippable throughout.

- **Merge libtypio into typio-linux as a monorepo.** Rejected:
  libtypio is consumed by 5 external engine repos and
  typio-settings. Putting it inside one consumer inverts the
  dependency stack and forces every engine to depend on the host
  repo. See D1.

- **Promote the whole `/home/ming/projects/typio/` workspace to a
  single git repo.** Rejected: the engines and typioctl have
  independent release cadences and contributor surfaces. The
  overhead of one version bump touching every component outweighs
  the atomic-commit benefit; path-dep + sibling checkouts already
  give atomic dev workflows.

- **Use the `wayland-protocols` crate for the protocols it ships
  (foreign-toplevel, fractional-scale, viewporter).** Rejected for
  consistency: splitting "protocols from crate" vs "protocols from
  local XML" produces two codegen paths and an inconsistent
  authoritative-XML story. See D2. Also moot for the two critical
  protocols (input-method-v2, virtual-keyboard-v1) which aren't in
  the crate anyway.

- **Consume libtypio through its C ABI from the Rust host (via
  `extern "C"` blocks).** Rejected: this would preserve every
  string-marshalling, raw-pointer, opaque-handle cost the migration
  is meant to eliminate, and give up the architectural payoff that
  justifies the port. See D3.

- **Defer until libtypio's `set_instance(*mut)` leak is fixed.**
  Rejected: the leak is one method on `EngineRegistry` and can be
  worked around with a stub `TypioInstance` during early phases.
  Blocking the entire migration on a libtypio refactor would
  serialise work that can otherwise proceed in parallel. The
  refactor is tracked separately in libtypio.

## Consequences

- Positive: the C ABI stops being a maintained surface for host
  consumption. Future `libtypio` API growth happens in native Rust;
  the C ABI grows only when an engine-plugin contract change
  requires it.

- Positive: debugging the keyboard/panel state machines gets the
  full Rust toolchain — exhaustive enum matching on
  `EngineAvailability` / `KeyProcessResult` / `WEnum<...>`,
  `Result<T, EngineError>` instead of out-params + bool, borrow
  checking instead of ASan use-after-free hunting.

- Positive: the daemon's eventual binary is built by a single
  `cargo build`, dropping the meson/cargo split entirely once flux
  is gone or cargo-native.

- Trade-off: two build systems coexist for the duration of the
  migration. Contributors building the daemon need both meson and
  cargo installed. CI must run both. This is the cost D4 accepts
  for keeping the daemon shippable.

- Trade-off: tests are duplicated during the migration — each ported
  subsystem keeps its C tests until the Rust replacement is
  verified, then the C tests are deleted (their Rust equivalents
  having been written first). The repo will temporarily carry both.

- Trade-off: the `EngineRegistry::set_instance(*mut TypioInstance)`
  leak forces the early Rust host to construct a stub
  `TypioInstance` (or skip the relevant code paths) until
  libtypio's refactor lands. This is a localized hack documented
  inline, not an architectural compromise.

- Negative (accepted): the C state-machine code's accumulated
  compositor-quirk knowledge (terminal-caret-rect behaviour,
  startup key-press storms, keymap-cancel windows, etc.) must be
  re-derived or carefully ported. Each one is a potential
  regression. Mitigation: port state machines in isolation with
  property-based tests before wiring them into the live event loop.

- Negative (accepted): the ship date for "pure-Rust host" is
  uncertain. The migration is paced by verification, not by
  deadline. v0.4.x releases continue to ship the C daemon until
  the Rust host reaches feature parity.

## Related

- [ADR-0002](0002-wayland-input-method-v2.md) — the host protocol
  this migration continues to target.
- [ADR-0021](0021-systemd-user-service-daemon-lifecycle.md) — the
  systemd user service that wraps whatever binary the host ends up
  being; unchanged by this ADR.
- [ADR-0030](0030-engine-process-manifests.md) — the engine plugin
  contract that defines what the C ABI must keep supporting even
  after the host stops consuming it.
- The `libtypio` repository's ADR set for framework-core decisions
  (engine trait hierarchy, registry semantics) that this migration
  depends on but does not modify.
