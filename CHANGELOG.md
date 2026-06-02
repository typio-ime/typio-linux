# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
