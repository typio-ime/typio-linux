# Project Scope: typio-linux vs. libtypio

This document clarifies what typio-linux is responsible for, what it is not,
and where the boundary with [libtypio](../../libtypio) lies. It is intended to
prevent the common confusion between the two projects and to explain why
typio-linux carries a UX responsibility that neither a generic OS adapter nor
a pure business-logic library could fulfill alone.

## The Two Projects

| | typio-linux | libtypio |
|---|---|---|
| **Role** | Platform host (OS adapter) | Core framework (business logic) |
| **Language** | C23 | Rust + hand-written C ABI |
| **Knows about** | Wayland, Vulkan, D-Bus, PipeWire, Linux filesystem | Engines, input contexts, config schema, engine registry |
| **Does not know about** | How an engine processes a key; what Rime schema is active | What compositor is running; what GPU renders the panel; what audio hardware is attached |
| **Contains engines** | No — discovers engine manifests and starts engine processes at runtime | No — provides the engine ABI and registry; engines are separate repos |
| **Process** | `typio` daemon binary | Linked as a static/shared library inside `typio` |

The dependency direction is one-way: typio-linux links libtypio and calls its
APIs. libtypio never reaches back into the host except through callbacks the
host registers.

## The Boundary

The contract between the two layers is defined by three surfaces in libtypio's
public headers:

### Surface 1: Instance lifecycle (`typio/runtime/instance.h`)

The host creates a `TypioInstance`, provides engine directories and an engine
discovery callback, then drives init and shutdown. libtypio never scans engine
paths; it calls back into the host's `TypioPluginLoaderFunc` once per engine
directory, and the host does manifest parsing, capability negotiation, and IPC
registration.

### Surface 2: Input context (`typio/abi/input_context.h`)

The host creates contexts, feeds `TypioKeyEvent` structs in, and registers
callbacks for text output. When an engine produces text, it calls
`typio_input_context_commit` or `typio_input_context_set_composition`; libtypio
fires the host's callbacks synchronously on the same call stack; the host
translates those into Wayland protocol calls (`commit_string`,
`set_preedit_string`, etc.).

This is the bidirectional seam: keys flow **in** from the compositor through
libtypio to the engine; text and compositions flow **out** from the engine
through libtypio back to the compositor.

### Surface 3: Observer callbacks (`typio/runtime/instance.h`)

The host registers callbacks for engine activation, mode changes, and status
icon updates. libtypio fires these when its internal state changes; the host
updates the tray, panel, and IPC event subscribers.

```
┌──────────────────────────────────────────────────────────┐
│  Engine Process                                           │
│  Implements Typio Engine Protocol                          │
│  Emits COMMIT / COMPOSITION response lines                 │
└──────────────┬──────────────────────┬────────────────────┘
	               │ engine request       │ engine responses
               ▼                      │
┌──────────────────────────────────────┴────────────────────┐
│  libtypio                                                  │
│  Instance, Registry, InputContext                          │
│  Engine lifecycle, key routing, config schema              │
│  No dlopen, no display server, no GPU, no audio            │
└──────────────┬─────────────────────────────────────────────┘
               │ host API + observer callbacks
               ▼
┌────────────────────────────────────────────────────────────┐
│  typio-linux                                             │
│  Wayland input-method, Vulkan panel, event loop            │
│  Manifest discovery, tray, IPC, voice capture, config watch│
└────────────────────────────────────────────────────────────┘
```

## What typio-linux Owns

### OS capability abstraction

typio-linux adapts the Linux/Wayland desktop to the portable interfaces
libtypio defines:

| OS capability | typio-linux module | Abstracted as |
|---|---|---|
| Keyboard input | Wayland keyboard grab → `TypioKeyEvent` | Input context key feed |
| Text output | Engine callbacks → `zwp_input_method_v2` commit/preedit | Compositor protocol |
| Popup positioning | `zwp_input_popup_surface_v2` | Panel surface |
| GPU rendering | Vulkan via flux canvas library | Candidate panel |
| Audio capture | PipeWire → `TypioVoiceSession.feed_audio` | Voice input |
| System tray | D-Bus StatusNotifierItem | Engine status indicator |
| Desktop notifications | D-Bus `org.freedesktop.Notifications` | Health alerts |
| External control | UDS + JSON-RPC 2.0 (TIP v1) | IPC bus |
| Plugin loading | `dlopen` + capability negotiation | Registry registration |
| Config persistence | File watch + debounced reload | Runtime config |
| Per-app identity | Store/restore engine and mode by `app_id` | Instance state |

### UX consistency

Because typio-linux sits between the operating system and the user, it bears
a responsibility that neither layer above (compositor) nor below (libtypio)
can fulfill: **ensuring a consistent, responsive user experience across all
the surfaces the user actually sees and touches.**

This is not cosmetic polish tacked on at the end. The UX responsibility is
structural:

- **Input responsiveness.** The event loop is single-threaded. Every GPU
  frame, D-Bus dispatch, config reload, and voice audio buffer competes with
  key processing for the same loop tick. The host must bound every non-input
  operation so a keypress never waits behind a swapchain rebuild, a glyph
  upload, or a compositor stall. See [Event Loop Scheduling](event-loop-scheduling.md)
  and [Vulkan and Flux Rendering](vulkan-flux-rendering.md).

- **Visual consistency.** The candidate panel, preedit decoration, tray icon,
  and mode indicator must all reflect the same state at the same time. The
  `TypioStateController` observer pattern ensures every surface reads from one
  source of truth. See [Control Surfaces](control-surfaces.md).

- **Crash recovery.** A compositor lock, DPMS-off, or system suspend must not
  corrupt committed text or freeze the input path. The focus controller derives
  state from facts every step, so recovery is the same code path as normal
  operation.
  See [Input-Method Session](input-method-session.md).

- **Cross-engine smoothness.** Switching engines must clear stale preedit and
  panel state before the new engine activates, so no ghost text survives an
  engine boundary. Shortcut bypass, modifier buffering, and key epoch fencing
  prevent keys from leaking between engines or between the IME and the
  application.

These are not features of libtypio — they are emergent properties of how the
host bridges OS capabilities to the framework. A different host (macOS,
Windows, Android) would face different OS surfaces but the same UX invariants.

## What libtypio Owns

libtypio is the platform-neutral core. It provides:

- `TypioInstance` — lifecycle, config, per-app identity
- `TypioRegistry` — engine listing, activation, switching
- `TypioInputContext` — key dispatch to engines, composition aggregation
- Engine ABI (`typio/abi/`) — the contract plugins implement
- Config schema — shared key vocabulary for hosts, engines, and control panels

libtypio does not contain engines, does not open shared libraries, does not
talk to any display server, and does not render pixels. Any code that would
only make sense on one operating system belongs in the host.

## What Belongs Where

When deciding where a change belongs, apply these tests:

| Question | If yes, it belongs in typio-linux |
|---|---|
| Does it require a Wayland protocol object? | Yes |
| Does it require GPU rendering or a GPU resource? | Yes |
| Does it require D-Bus, PipeWire, inotify, or epoll? | Yes |
| Does it affect the panel layout, theme, or visual appearance? | Yes |
| Does it affect how fast a keypress becomes a visible result? | Yes |
| Does it affect how the tray icon or IPC surface reports state? | Yes |
| Does it discover, load, or unload an engine `.so` file? | Yes |

| Question | If yes, it belongs in libtypio |
|---|---|
| Does it affect how keys are routed to engines? | Yes |
| Does it affect the composition data model? | Yes |
| Does it affect engine lifecycle (init, destroy, focus, reset)? | Yes |
| Does it define a config key that any host or engine must understand? | Yes |
| Does it add a new engine capability or vtable method? | Yes |

| Question | If yes, it belongs in an engine plugin |
|---|---|
| Does it implement a specific input method (Rime, Pinyin, voice)? | Yes |
| Does it process key events and produce text? | Yes |
| Does it maintain its own dictionary, model, or schema? | Yes |

## Common Confusion Points

**"I want to add a new input method."** Write an engine plugin against
`typio/abi/abi.h`. Neither typio-linux nor libtypio needs to change.

**"I want the panel to show a new kind of content."** This is typio-linux.
The panel content model (`TypioPanelContent`) is GPU-free and testable, but
the surface, rendering, and positioning are Wayland-specific.

**"I want to change how engines are switched (Ctrl+Shift, next/prev)."** The
trigger mechanism (keyboard shortcut detection, modifier buffering) is in
typio-linux. The registry operation (`next_keyboard`) is in libtypio.

**"I want to add a new config key."** If the key is consumed by engines, define
it in libtypio's config schema. If the key controls a host behavior (panel
font, tray visibility, GPU options), it belongs in typio-linux's runtime
config.

**"I want to port Typio to macOS."** Write a new host that links libtypio,
provides a `TypioPluginLoaderFunc`, feeds keys, and renders the panel.
libtypio stays the same; engine plugins stay the same.

## See also

- [Panel Architecture](panel-architecture.md) — the UI surface typio-linux renders.
- [Control Surfaces](control-surfaces.md) — tray, IPC bus, state controller.
- [Event Loop Scheduling](event-loop-scheduling.md) — how the event loop preserves responsiveness.
- [Input-Method Session](input-method-session.md) — session lifecycle and recovery paths.
- [Vulkan and Flux Rendering](vulkan-flux-rendering.md) — GPU rendering and performance.
- [Wayland Input Method Protocol](wayland-input-method.md) — the protocol layer.
