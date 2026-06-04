# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  typio-wayland's own ADR-0012 (shared glyph atlas). Fixed digit key range
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
