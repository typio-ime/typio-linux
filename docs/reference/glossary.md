# Glossary

Canonical terms used across the typio-wayland codebase and documentation.
Each entry links to the primary source where the term is defined or discussed
in depth.

## Core UI

| Term | Definition | Primary source |
|------|-----------|----------------|
| **Panel** | The single floating IME UI surface. Aggregates candidates, preedit decoration, status banners, and future toolbar/waveform zones. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Panel Surface** | The Wayland/Vulkan presentation object behind the Panel. Owns the `zwp_input_popup_surface_v2`, swapchain, present/recover loop, scale, and output tracking. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Panel Content** | Display-agnostic data describing what the Panel should show. Contains no Wayland or GPU types. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Zone** | A bounded area inside Panel Content (Candidate, Preedit, Status, future Toolbar). A zone is a concrete triple: a content fragment, its geometry fragment, and the painter that draws it. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Panel Geometry** | The immutable positioned snapshot produced by the Layout step from Panel Content + Theme + Scale. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |

## Frontend policy

| Term | Definition | Primary source |
|------|-----------|----------------|
| **Panel Producer** | A frontend subsystem that requests Panel content. Current producers: candidate composition, indicator, voice. | [Panel Architecture](../explanation/panel-architecture.md) |
| **UI Owner** | The producer currently allowed to control Panel visibility. Exactly one owner is visible at a time. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Panel Coordinator** | Frontend policy layer that arbitrates owners, pending positioned UI, and anchor readiness. Not renderer code. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Position Anchor** | The current activation's trusted placement state for the input-popup surface. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Anchor Probe** | A one-shot no-op input-method commit (`set_preedit_string("", -1, -1); commit`) used to ask clients for a fresh caret rectangle. | [Panel Architecture](../explanation/panel-architecture.md) |
| **Anchor Readiness** | Whether the current activation's position anchor can be trusted (compositor sent `text_input_rectangle`, or candidates successfully presented). | [Panel Architecture](../explanation/panel-architecture.md) |

## Rendering pipeline

| Term | Definition | Primary source |
|------|-----------|----------------|
| **Layout** (step) | Pure function: `PanelContent + Theme + Scale → PanelGeometry`. Not the same as `TextShape`. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |
| **Painter** | Walks PanelGeometry, emits draw commands, blits TextShapes onto a Canvas/Frame. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |
| **TextShaper** | String + font → colour-independent coverage mask + metrics. Replaces the old `TypioTextEngine`. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |
| **TextShape** | A shaped glyph run produced by the TextShaper. Replaces the old `TypioTextLayout`. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |
| **RenderDevice** | Shared GPU context used by the Panel rendering pipeline. | [ADR-0014](../adr/0014-canonical-panel-vocabulary.md) |
| **Glyph Atlas** | A single persistent R8 texture holding every rasterised glyph, keyed `(font_id, glyph_id)`, packed by a skyline allocator. | [ADR-0012](../adr/0012-glyph-atlas-shared-texture.md) |
| **Atlas Reclamation** | Wholesale rebuild of the glyph atlas (texture, hash table, packer, counts) when the hash load crosses 75 % **or** the shelf packer runs out of texture space; the next draw re-rasterises the visible page lazily. | [ADR-0020](../adr/0020-atlas-reclamation-and-glyph-layer-modularization.md) |

## Wayland input method

| Term | Definition | Primary source |
|------|-----------|----------------|
| **input-popup surface** | The `zwp_input_popup_surface_v2` protocol role — how the compositor positions the Panel near the text cursor. Not a synonym for "Panel" or "popup". | [Wayland Input Method](../explanation/wayland-input-method.md) |
| **Virtual Keyboard** | `zwp_virtual_keyboard_v1`. Forwards unhandled keys back to the compositor as synthetic press/release events. | [Wayland Input Method](../explanation/wayland-input-method.md) |
| **Keyboard Grab** | `zwp_input_method_keyboard_grab_v2`. Delivers raw key/modifier/keymap events for the focused input context. | [Wayland Input Method](../explanation/wayland-input-method.md) |
| **Activation** | A `zwp_input_method_v2.activate` event. Begins a focused input session. | [Timing Model](../explanation/timing-model.md) |
| **Grab Generation** | A generation counter that fences stale key events. A key cycle belongs to the current grab only if the daemon observed its press in the current generation. | [Timing Model](../explanation/timing-model.md) |
| **Reactivation** | A repeat `activate` for an already-focused session. Keeps the grab and composition, re-anchors the Panel. Classified at `done`. | [ADR-0018](../adr/0018-focus-transition-classification.md) |
| **Boundary Bridge** | Policy layer for short-lived handoff behavior at activation boundaries (orphan release forwarding, carried modifiers). | [Maintenance Manual](../dev/maintenance.md) |

## Engine and config

| Term | Definition | Primary source |
|------|-----------|----------------|
| **Engine Plugin** | A `libtypio_engine_<name>.so` shared library loaded via `dlopen`. Implements the libtypio engine ABI. | [Engine Discovery](engine-discovery.md) |
| **Engine Availability** | The active engine lifecycle state that answers whether the engine can process input right now. Non-ready keyboard engines consume key cycles locally instead of leaking raw keys to the application. | [Timing Model](../explanation/timing-model.md) |
| **TIP v1** | Typio IPC Protocol version 1. Unix Domain Socket + length-prefixed JSON-RPC 2.0. | [IPC Protocol](ipc-protocol.md) |
| **Reconciler** | The lifecycle repair layer that compares observed axes with the declared phase and fixes resource drift. | [Timing Model](../explanation/timing-model.md) |

## Vocabulary to avoid

| Avoid | Use instead | Reason |
|-------|-------------|--------|
| popup (as a concept noun) | **Panel** | `popup` is the Wayland positioning role, not the UI itself |
| popup surface, window | **input-popup surface** | Names the protocol role precisely |
| UI source, caller | **Panel Producer** | States who requests content |
| active UI, current status | **UI Owner** | Explains visibility authority |
| UI manager, frontend UI | **Panel Coordinator** | Describes arbitration without implying rendering |
| popup position, cursor position | **Position Anchor** | Anchor is the trustworthy placement state |
| fresh rect, valid position | **Anchor Readiness** | Describes whether the anchor can be trusted |
| refresh hack, browser workaround | **Anchor Probe** | Describes the explicit no-op commit mechanism |
