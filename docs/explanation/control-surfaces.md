# Control Surfaces

This document covers typio-wayland's user-facing control surfaces:

- the tray menu
- the D-Bus status interface consumed by external control panels
- future settings widgets or shell integrations

The broader ownership model for persisted config, runtime state, and staged edits is defined in libtypio's config system.

## Scope

Control surfaces have two jobs:

- present runtime state coming from the typio daemon
- let the user edit persistent configuration safely

They must not become a second source of truth for runtime or config state.

## Sources Of Truth

For UI work, the practical rules are:

- runtime state must come from the daemon, not from client-side filesystem guesses
- if a runtime selection is unknown, keep the widget unselected instead of guessing a fallback entry
- persistent edits must start from the daemon's current `ConfigText`
- widget state is never authoritative by itself
- programmatic refresh must not overwrite newer local staged edits
- selector widgets should prefer shared binding models over bespoke sync logic

## Editing Model

External control surfaces use an instant-apply model with background autosave:

1. wait for the first `ConfigText` from the status bus
2. seed the local stage from that config text
3. let user edits update widget state immediately
4. mirror the edited form back into the staged full config text
5. schedule an automatic `SetConfigText` submission after a short debounce

Required invariants:

- Before the first successful seed, widget initialization must not write staged config.
- During programmatic refresh, all change handlers must be suppressed.
- UI response must be immediate; persistence is allowed a short async debounce.
- Only one config write may be in flight at a time.
- If the user edits again while a write is in flight, the newest staged config must win once the current write finishes.
- Old daemon replies must not overwrite newer local staged edits.
- Default values belong to schema application and daemon-side config reload, not to control-surface startup.

## Known Failure Pattern

This class of bug is easy to reintroduce:

1. the control surface starts before the daemon is ready
2. widget setup emits change signals
3. the UI writes a local stage based on widget defaults
4. the user edits one unrelated setting
5. the whole polluted staged config overwrites unrelated daemon config

This is how a Rime-schema edit can accidentally overwrite unrelated engine
settings or runtime state.

## Information Architecture

- Top-level navigation should represent stable product areas such as `Appearance`, `Input engines`, and `Shortcuts`.
- Avoid mixing categories and concrete instances in the same navigation layer.
- Engine/backend/model choices belong in dropdowns, not in extra tabs.
- Use at most two navigation levels in the control center.
- Keyboard and voice are engine categories, not competing alternatives. The control panel should show them as separate sections in the same product area, because they can be active at the same time.

## Tray Menu Rules

- The main engine list should contain keyboard engines only.
- Rime schema choices may appear under a Rime-specific submenu because they are part of day-to-day keyboard usage.
- Voice controls should stay out of the tray unless they become a primary frequent action.
- The tray icon should represent keyboard-engine status, because keyboard engines own composition and status icons. Voice state may appear in tooltip or structured status surfaces, but it must not replace the keyboard icon.

## Documentation And Tests

Any change to control-surface behavior should update:

- this document, if source-of-truth or editing rules change
- user documentation, if visible UI or behavior changes
- regression tests for startup seeding, dirty-state handling, and config apply

Minimum regression coverage to keep:

- startup before the daemon appears must not dirty the local stage
- programmatic dropdown refresh must not rewrite config
- changing one field must not rewrite unrelated top-level settings
- fast repeated switch toggles must preserve the newest local state
- delayed daemon replies must not overwrite newer staged edits
- Rime and voice settings must round-trip through daemon `ConfigText`
