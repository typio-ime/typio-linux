# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **flux dependency pinned to v0.1.0** (first standalone release). Bumped
  `subprojects/flux.wrap` from the dangling `v0.0.8` pin — that tag never
  existed in the flux repo, so the wrap was silently failing and the build
  was resolving to whatever `flux.pc` pkg-config found first. Added an
  explicit `version: '>= 0.1.0'` floor on the `dependency('flux')` lookup
  so a stale system install can no longer mask a broken wrap.

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
- Eliminate candidate popup navigation lag that appeared after extended sessions. The popup now remains responsive during up/down/pageup/pagedown navigation regardless of session duration or compositor state (post-lock/suspend, DPMS events). Three complementary fixes: reduced acquire timeout from 32ms to 2ms, deferred panel flush when retry is pending, and persistent glyph upload context to reduce per-glyph overhead. (ADR-0015)

## [0.0.4] — 2026-05-29

### Fixed
- Stop swallowing genuine keystrokes after a keyboard-grab rebuild. The startup stale-key guard suppressed every press within two Wayland dispatch epochs of a grab rebuild; on the reactivation path (which terminals and tmux trigger frequently) this had nothing legitimate to suppress and ate the user's first real keystroke. Stale presses are now dropped by the grab-generation fence instead, and the startup guard only bounds orphan-release cleanup.

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
