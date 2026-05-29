# ADR-0005: Unified Panel Backend for Candidate and Status UI

- **Status**: Accepted
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

The host has one visual floating UI: the candidate popup (`zwp_input_popup_surface_v2` rendered via flux/Vulkan). All other user-facing state historically had no proper home:

- Voice recording / processing state was injected into the preedit string and committed through the Wayland input-method protocol — polluting the application's input stream and depending on client preedit styling that may be invisible.
- The "text UI backend" abstraction only knew how to render a candidate popup. There was no content model letting other subsystems (voice, tray, future features) contribute visual state.

As the host grows, more floating UI is expected: voice waveform visualisation, handwriting pads, quick-phrase palettes. Each should not acquire its own ad-hoc Wayland surface.

## Decision

Evolve the candidate popup into a **unified panel backend** that accepts a generic content model and composites multiple zones inside a single surface.

- **Phase 1**: add a `status` zone to the popup surface. Migrate voice recording / processing / error indicators from preedit injection to the status zone. Keep existing APIs compatible; only add `show_status()` / `hide_status()`.
- **Phase 2**: formalise a `TypioPanelContent` content model aggregating data from `TypioInputContext`, voice service, and other subsystems. The frontend becomes an aggregator that builds `TypioPanelContent` and pushes it to the panel backend.
- **Phase 3**: split the internal popup into explicit `PanelZone`s (candidates, preedit decor, status, toolbar) with independent layout and paint modules.
- **Phase 4**: if free-floating panels are needed (settings window, waveform overlay), introduce a `LayerShellProvider` as an alternative `SurfaceProvider` without changing the content model or composer.

### Constraints

- A single `zwp_input_method_v2` can only create one `zwp_input_popup_surface_v2`. Candidate and status UI must therefore share the surface, rendered as distinct zones by the composer.
- The content model (`TypioPanelContent`) must remain free of Wayland or GPU types so it can be unit-tested without a display server.
- Layout and paint must stay decoupled: layout is a pure function from content + config → geometry; paint is a pure recorder from geometry → draw commands.

## Alternatives considered

- **Keep preedit injection for voice status.** Rejected: couples voice state to the input-method protocol, creates visual inconsistency across clients, and blocks the preedit channel from being used for real text while voice is active.
- **Give voice its own `wl_surface` or layer-shell surface.** Rejected for Phase 1. A second surface adds protocol complexity, extra GPU resources, and positioning logic. Voice status is transient and small; it belongs near the cursor. A separate surface may be reconsidered in Phase 4 for large free-floating panels.
- **Use the system tray (SNI) for recording indication.** Rejected as primary UI: too far from the user's focus during typing. The tray remains as a secondary indicator.

## Consequences

- **Positive**: voice state no longer pollutes the preedit protocol stream. Visual appearance is fully controlled by the host (theme, font, colour, HiDPI). Future UI features hook into the same backend.
- **Positive**: the existing flux GPU pipeline, font cache, layout cache, and theme system are reused.
- **Trade-off**: `zwp_input_popup_surface_v2` visibility depends on the compositor providing a valid `text_input_rectangle`. A compositor that hides the popup when no text input is focused would also hide the voice indicator. Acceptable because the host holds a keyboard grab during PTT and the input-method session is generally active.
- **Trade-off**: the popup code carries dual responsibility (candidates + status) until Phase 3 zone refactoring is complete.
- **Negative (accepted)**: Phase 1 reuses the preedit text layout path for status text, so status and preedit share the same colour slot until Phase 2/3 add a dedicated status colour.
