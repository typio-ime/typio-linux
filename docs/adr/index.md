# Architecture Decision Records

ADRs are append-only records of significant design decisions in `typio-wayland` — the Wayland host (`typiod`) for the Typio input method. Once accepted, they are not edited; a new ADR supersedes an old one if a decision changes.

Decisions about the framework core (engine ABI, composition contract) live in the `libtypio` repository's ADR set.

> **Candidate-popup lag — reading guide for ADR-0006 / 0010–0013.** The popup's
> candidate-switch lag was diagnosed in four passes, and the first three each
> *mis-attributed* the root cause. The actual cause was a **per-page swapchain
> rebuild** (ADR-0013). The earlier ADRs remain valid as **independent**
> improvements — non-blocking present (0010), colour-independent coverage glyphs
> (0011), shared glyph atlas (0012) — each fixes a real problem and is still in
> force; only their "this is what cured the lag" claim was wrong, and each now
> carries a scope-correction block. The misdiagnosis trail is kept deliberately:
> it is the record that stops the next reader repeating it. See
> [`docs/dev/maintenance.md`](../dev/maintenance.md) for the condensed lesson.

| ADR | Title | Status |
|-----|-------|--------|
| [ADR-0001](0001-record-architecture-decisions.md) | Record Architecture Decisions | Accepted |
| [ADR-0002](0002-wayland-input-method-v2.md) | Adopt `zwp_input_method_v2` as the host protocol | Accepted |
| [ADR-0003](0003-session-controller-reduce-diff.md) | Session controller — derived state, idempotent diff | Accepted |
| [ADR-0004](0004-event-loop-scheduling-and-watchdog.md) | Event-loop scheduling and watchdog | Accepted |
| [ADR-0005](0005-unified-panel-backend.md) | Unified panel backend for candidate and status UI | Accepted (vocabulary formalised by ADR-0014) |
| [ADR-0006](0006-resilient-candidate-popup-present.md) | Resilient candidate-popup GPU present | Accepted (amended by ADR-0010) |
| [ADR-0007](0007-dbus-adapter-over-status-service.md) | D-Bus adapter as a thin transport over `TypioStatusService` | Superseded by ADR-0008 |
| [ADR-0008](0008-ipc-protocol-resource-namespaces-uds-only.md) | TIP v1 — IPC Protocol with resource namespaces, UDS-only, push events | Accepted |
| [ADR-0009](0009-long-term-performance-optimizations.md) | Long-term performance optimizations — font cache purging, composition short-circuit, and snapshot fast-path | Accepted |
| [ADR-0010](0010-non-blocking-candidate-popup-present.md) | Non-blocking present mode for the candidate popup | Accepted (lag-cause attribution corrected by ADR-0013) |
| [ADR-0011](0011-colour-independent-coverage-glyphs.md) | Colour-independent coverage glyph textures (draw-time tint) | Accepted (texture model superseded by ADR-0012; lag-cause attribution corrected by ADR-0013) |
| [ADR-0012](0012-glyph-atlas-shared-texture.md) | Shared glyph atlas (rasterise once, reference sub-rects) | Accepted (lag-cause attribution corrected by ADR-0013) |
| [ADR-0013](0013-grow-only-popup-swapchain.md) | Grow-only popup swapchain (stop rebuilding per candidate page) | Accepted |
| [ADR-0014](0014-canonical-panel-vocabulary.md) | Canonical panel vocabulary and module ontology | Accepted (refines ADR-0005) |
| [ADR-0015](0015-candidate-popup-lag-final-fixes.md) | Candidate popup lag — final fixes (acquire timeout, retry deferral, persistent upload context) | Accepted |

## Looking for something else?

- Framework core ADRs: see the `libtypio` repository
- CLI ADRs: see the `typioctl` repository
- Settings-panel ADRs: see the `typio-settings` repository
