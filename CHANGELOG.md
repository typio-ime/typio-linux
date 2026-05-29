# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
