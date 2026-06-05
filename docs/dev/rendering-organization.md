# Rendering Code Organization

This document defines where UI and rendering code belongs. It is a contributor
standard, not a rendering deep dive. For the drawing pipeline, see
[Frontend Graphics](../explanation/frontend-graphics.md). For Panel vocabulary,
see [Panel Architecture](../explanation/panel-architecture.md).

## The rule

Organize code by **responsibility**, not by the library it happens to call.

The allowed dependency direction is:

```text
frontend policy
    -> UI model
    -> layout
    -> paint recording
    -> presentation backend
```

Higher layers may depend on lower layers only through the layer's public API.
Lower layers must not call back into frontend policy.

## Source placement

| Code kind | Owns | Does not own | Current home |
|---|---|---|---|
| Frontend policy | Focus, input-method commits, owner arbitration, anchor readiness, voice/indicator state | Panel geometry, paint, GPU frames | `src/wayland/` |
| UI model | Data-only description of what should be shown | Wayland objects, GPU objects, compositor timing | `src/ui/panel/content.h`, pure helpers under `src/ui/` |
| UI planning helpers | Small, testable decisions about preedit/panel flushes and positioned UI timing | Rendering, protocol writes, ownership side effects | `src/ui/state.*`, `src/ui/preedit.*` |
| Layout | `Content + config + palette + scale -> geometry` | Frame lifecycle, present, owner policy | `src/ui/panel/layout.*` |
| Theme/config shaping | Panel visual parameters and palette resolution | Runtime policy, protocol state, GPU allocation | `src/ui/panel/theme.*`, config loading in `layout.*` |
| Paint recording | Geometry to drawing commands | Surface creation, acquire/present, retries, owner policy | `src/ui/panel/paint.*` |
| Presentation backend | Wayland surface role, scale/output tracking, swapchain, frame lifecycle, stall recovery | What to draw, who owns the Panel | `src/ui/panel/surface.*` |
| Render device | Process-wide graphics device and backend bootstrap | Panel content, layout, frontend state | `src/ui/panel/device.*` |
| Text shaping backend | Font lookup, shaping, glyph atlas, backend-specific text fill | Candidate/indicator/voice policy | `src/ui/panel/text_shaper.*` (orchestrator) over `font_resolve.*`, `font_cache.*`, `glyph_atlas.*`, `glyph_upload.*`, `glyph_pack.*` |
| Disabled-backend stub | API compatibility when the graphics backend is unavailable | Any real rendering behavior | `src/ui/panel/stub.c` |

## Directory standard

### `src/wayland/`

Use this for policy that observes or changes the input-method session.

Examples:

- selecting the current `UI Owner`;
- deciding whether a positioned status UI must wait for a trusted anchor;
- sending the no-op anchor probe;
- translating voice/session events into Panel requests.

Frontend code may ask the Panel Coordinator to show or hide content. It should
not build geometry, record paint, allocate swapchains, or call flux.

### `src/ui/`

Use this root only for UI-domain helpers that are independent of one concrete
Panel surface.

Allowed examples:

- preedit formatting;
- UI update planning;
- small pure helpers with unit tests;
- renderer-adjacent algorithms that do not own a surface, such as glyph
  packing or fallback-font cache policy.

Avoid adding large product UI modules directly under `src/ui/`. Put concrete
surfaces under a named child directory such as `src/ui/panel/`.

### `src/ui/panel/`

Use this for the Panel implementation.

Keep the subroles clear:

| File role | Standard |
|---|---|
| `content.*` | Data only. No Wayland, no GPU, no frontend structs. |
| `layout.*` | Pure geometry and caches. No frame lifecycle. |
| `paint.*` | Record commands into the selected canvas. No acquire, submit, present, hide, or retry policy. |
| `panel.*` | Orchestrates Panel content, theme, geometry, visible state, and the surface API. No frontend owner arbitration. |
| `surface.*` | Owns the protocol surface and presentation lifecycle. Does not decide what content means. |
| `device.*` | Owns backend device creation and sharing. Does not know about Panel content. |
| `text_shaper.*` | Orchestrates shaping + draw over the font/glyph modules below; owns the `TypioTextShape` and the panel-facing atlas facade. Does not know about owners or input sessions. |
| `font_resolve.*` | Fontconfig: descriptor parse, family→file, per-codepoint fallback. One Fontconfig resolution per key, then cached. |
| `font_cache.*` | `FT_Face` + `hb_font_t` object cache, keyed `(path, size, weight)` with a monotonic `font_id`. |
| `glyph_atlas.*` | Shared R8 coverage texture, the `(font_id, glyph_id)` hash table, and atlas reclamation. |
| `glyph_upload.*` | Persistent Vulkan staging context for glyph sub-region uploads. |
| `glyph_pack.*` | Skyline shelf packer (pure, unit-tested). |
| `theme.*` | Visual palette defaults and detection only. |
| `stub.c` | Mirrors the public Panel API for no-backend builds. |

## Backend naming

Use **backend** for a concrete graphics implementation such as flux/Vulkan.
Use **presentation backend** for code that binds a rendered frame to a Wayland
surface and presents it.

Do not name product UI concepts after the backend:

| Prefer | Avoid |
|---|---|
| `Panel` | `FluxPanel` |
| `Panel Surface` | `FluxSurface` when it means the product surface |
| `Paint Target` | `Flux UI` |
| `RenderDevice` | global `ui_device` |

It is fine for a backend adapter to expose backend types at its boundary, such
as `PanelPaintTarget` containing a `flux_canvas`. That boundary should remain
small and easy to replace.

## Dependency rules

1. `src/wayland/` may depend on `src/ui/panel/panel.h`, not on
   `layout.h`, `paint.h`, `surface.h`, `device.h`, or `text_shaper.h`.
2. `content.h` must remain free of Wayland, flux, Vulkan, Fontconfig,
   HarfBuzz, and FreeType types.
3. `layout.*` may depend on abstract text-shaper handles, but must not present
   frames or allocate surfaces.
4. `paint.*` may depend on the selected canvas API, but must not acquire or
   present frames.
5. `surface.*` may depend on Wayland and flux/Vulkan because it is the
   presentation backend.
6. Backend-specific code must be behind `HAVE_FLUX` or a narrow API with a
   stub implementation.
7. Owner arbitration and anchor readiness belong to the Panel Coordinator, not
   to `src/ui/panel/`.

## Adding a new visual feature

Use this placement checklist:

| If you are adding... | Put it in... |
|---|---|
| A new producer, such as a voice or mode status | `src/wayland/`, routed through the Panel Coordinator |
| A new piece of data shown in the Panel | `TypioPanelContent` first |
| A new visual region | content zone + `layout.*` geometry + `paint.*` recording |
| A new theme knob | `theme.*`, `PanelConfig`, config docs |
| A new GPU resource or present behavior | `surface.*` or `device.*` |
| A new text shaping/cache rule | `text_shaper.*` or a pure helper under `src/ui/` |
| A new graphics backend | mirror `device.*`, `surface.*`, text shaping/fill, and the stub boundary |

The sequence should be content -> layout -> paint -> present. If an
implementation starts by adding frontend conditionals inside `paint.*` or
`surface.*`, the abstraction is probably in the wrong layer.

## Current refactor target

The current `src/ui` tree is acceptable, but the target shape is:

```text
src/ui/
  preedit.*              pure UI formatting
  state.*                pure UI planning
  panel/
    content.*
    layout.*
    paint.*
    panel.*
    surface.*
    device.*
    text_shaper.*         shaping + draw orchestrator
    font_resolve.*        fontconfig resolution + fallback memo
    font_cache.*          FT_Face + hb_font objects
    glyph_atlas.*         coverage texture + reclamation
    glyph_upload.*        Vulkan staging context
    glyph_pack.*          skyline packer
    theme.*
    stub.c
```

If more surfaces are added, create a new child directory instead of growing
`src/ui/panel/` into a general rendering toolbox. If the backend code grows
large enough to be shared by multiple surfaces, split it under a dedicated
backend directory only after there are at least two real consumers.
