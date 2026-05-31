# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
