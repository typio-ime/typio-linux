# ADR-0017: Positioned UI arbitration for panel owners

- **Status**: Accepted
- **Date**: 2026-06-01
- **Deciders**: typio-wayland maintainers

## Context

The Wayland host has one floating UI surface: the Panel. It is presented through
one `zwp_input_popup_surface_v2` role, but several independent producers want to
show content on it:

- keyboard composition shows candidates and preedit;
- engine/profile changes show a transient indicator;
- voice input shows loading, recording, processing, unavailable, and error
  statuses.

Before this decision, those producers called the Panel directly. That created
two classes of bugs.

First, ownership was implicit. The indicator has an auto-hide timer, while
candidates are driven by composition updates. If the user typed quickly after an
indicator appeared, the old indicator timer could later hide the candidate UI.
The surface looked like it had a rendering bug, but the real bug was stale UI
ownership.

Second, positioning readiness was producer-specific. Candidate UI usually
appeared in the right place in browsers because it was shown after the browser
and compositor had established the text-input rectangle. The indicator could be
shown earlier, before that anchor was trustworthy, and was therefore placed at a
stale or default position. Once candidates had appeared, a later indicator was
placed correctly because the Panel surface had already been anchored.

## Decision

Introduce a small frontend UI coordinator for every Panel producer. Producers no
longer own Panel visibility directly; they submit requests with an explicit UI
owner:

- `TYPIO_WL_UI_OWNER_CANDIDATE`
- `TYPIO_WL_UI_OWNER_INDICATOR`
- `TYPIO_WL_UI_OWNER_VOICE`

The coordinator lives in `src/wayland/panel_coordinator.c`. It is frontend
policy, not rendering code; `src/ui/panel/` remains the Panel renderer.

The coordinator enforces these rules:

1. Exactly one owner may be visible on the Panel at a time.
2. A later owner replaces the current owner.
3. A hide request only hides the owner that issued it.
4. Pending positioned UI is cancelled when candidates arrive.
5. The indicator timer can hide only the indicator owner, never candidates or
   voice status.

Positioned status UI also shares one anchor-readiness model. Each activation
gets a new position-anchor generation. That generation becomes ready when either
of these facts arrives:

- the compositor sends `zwp_input_popup_surface_v2.text_input_rectangle`;
- the candidate Panel successfully presents for the current activation.

Indicator and voice status requests wait for this anchor before mapping the
Panel. If the anchor never becomes ready within the bounded wait window, the
pending request is dropped rather than shown at a stale position.

When a positioned status request is queued without a ready anchor, the
coordinator may send one **anchor probe** for the current generation. The probe
is a no-op input-method commit (`set_preedit_string("", -1, -1)` followed by
`commit`) whose only purpose is to make clients that update caret rectangles on
input-method commits, notably browsers, send a fresh
`text_input_rectangle`. The probe is enabled by default and is bounded by
`display.anchor_probe_timeout_ms`.

Candidates are special: they are themselves an anchor-producing UI. A successful
candidate present marks the current anchor generation ready, which makes later
positioned status UI reuse the same trusted placement.

## Terminology

| Term | Meaning |
|---|---|
| **Panel** | The single floating IME UI surface. It can display candidates, indicator text, or voice status, but only one owner at a time. |
| **Panel producer** | A subsystem that requests Panel content: composition, indicator, or voice. |
| **UI owner** | The producer currently allowed to control Panel visibility. |
| **UI arbitration** | The coordinator rule that decides which owner is visible and which hide requests are valid. |
| **Positioned UI** | UI that must appear near the active text input area. |
| **Position anchor** | The current activation's trusted placement state for the input-popup surface. |
| **Anchor generation** | Monotonic activation token used to reject stale placement facts. |
| **Anchor readiness** | Whether the current generation has a trustworthy placement source. |
| **Anchor probe** | A one-shot no-op input-method commit used to ask the focused client/compositor for a fresh text-input rectangle. |
| **Pending positioned UI** | A status or indicator request queued until anchor readiness. |
| **Stale owner event** | A timer or release event from an owner that is no longer visible. It must not hide the current owner. |

## Alternatives considered

- **Keep direct Panel calls and add guards at each call site**: Rejected. It
  spreads ownership policy across input, voice, indicator, and event-loop code.
  The next UI producer would repeat the same bug class.
- **Assign static priority, such as candidates always beat voice and voice
  always beats indicator**: Rejected for now. The observed rule is temporal:
  the later UI intentionally replaces the current UI. Priority can be added
  later inside the coordinator if product policy changes.
- **Show indicator immediately and only cancel on obvious stale rectangles**:
  Rejected. Wayland input-method v2 does not give a serial that proves a
  `text_input_rectangle` belongs to the current activation. Generation-based
  readiness is the safer contract.
- **Create separate Wayland surfaces per UI type**: Rejected. The compositor
  already positions one input-popup surface for the active text input. Multiple
  surfaces would add stacking, focus, and lifetime ambiguity without solving
  stale ownership.

## Consequences

- Positive: The indicator auto-hide timer can no longer hide the candidate UI
  after the user starts typing.
- Positive: Browser placement becomes explicit: status UI waits for a trusted
  position anchor, candidate presentation can establish that anchor, and the
  default anchor probe can refresh it for out-of-band status UI.
- Positive: New Panel producers have one integration point and one vocabulary:
  owner, request, hide, pending, anchor readiness.
- Trade-off: Status UI may be skipped when the compositor never provides a
  trustworthy anchor and candidates never appear. This is preferable to showing
  at the wrong location.
- Trade-off: The anchor probe can create a harmless input-method commit in
  clients that otherwise would not update text-input state. It is configurable
  for compositors or clients that treat no-op commits poorly.
- Trade-off: The coordinator is another frontend state machine. It must remain
  small and should not absorb layout, painting, or engine logic.

## Related documents

- [ADR-0005: Unified panel backend](0005-unified-panel-backend.md)
- [ADR-0014: Canonical panel vocabulary and module ontology](0014-canonical-panel-vocabulary.md)
- [Frontend Graphics](../explanation/frontend-graphics.md)
- [Wayland Input Method Protocol](../explanation/wayland-input-method.md)
