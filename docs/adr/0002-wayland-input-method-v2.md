# ADR-0002: Adopt `zwp_input_method_v2` as the Host Protocol

- **Status**: Accepted
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

`typio-wayland` is a Wayland-native input method host. To receive activation, keyboard input, and popup placement, it must bind to a Wayland input-method protocol. At project start the only broadly supported option was `zwp_input_method_manager_v2` / `zwp_input_method_v2`, defined in `input-method-unstable-v2.xml`.

The protocol is explicitly **unstable**: compositor behaviour varies, event ordering is not fully specified, and it may be superseded by a future stable version.

## Decision

The host implements `zwp_input_method_v2` directly and treats the unstable status as a risk to be managed, not a blocker.

1. **Bind the protocol at runtime** via `zwp_input_method_manager_v2.get_input_method`. If the compositor does not advertise the manager, the host exits cleanly with a descriptive error.
2. **Defensive serial handling.** The `done` serial is a commit serial, not a sequence number. A serial of 0 means no `done` has arrived yet; the host refuses to commit preedit or text in that state to avoid silently dropped writes.
3. **Isolate protocol knowledge in one layer.** `wl_input_method.c` is the only file that speaks `zwp_input_method_v2`. The engine layer (in `libtypio`) knows nothing about Wayland. This limits the blast radius if the protocol changes.
4. **Pair with `zwp_virtual_keyboard_v1`** for forwarded keys. The grab and the vk keymap form one logical resource — see [ADR-0003](0003-session-controller-reduce-diff.md).

## Alternatives considered

- **Wait for a stable v3.** Rejected: no stable version existed at project start and no implementation timeline was available. Waiting would have left the host without a viable platform.
- **Use a D-Bus or portal fallback (ibus-style).** Rejected: the project is explicitly Wayland-native. A portal or D-Bus path would add a second platform abstraction, break compositor-integrated popup placement, and require maintaining two input paths.
- **Implement only `zwp_text_input_v3` and rely on the compositor to forward keys.** Rejected: that is the *client* side. As an input method, the host must bind the input-method side to receive activation and the keyboard grab.

## Consequences

- **Positive**: works on every compositor that supports `input-method-unstable-v2` (Sway, Hyprland, River, KWin, Mutter, …).
- **Positive**: the unstable status forced a defensive design (derived session state, serial chokepoint, see [ADR-0003](0003-session-controller-reduce-diff.md)) that improves robustness even on compositors with buggy v2 implementations.
- **Trade-off**: some compositors send `activate` without a matching `deactivate`, or deliver keys before the first `done`. The session controller handles these as facts that flow through the same reduce step.
- **Negative (accepted)**: if a future stable v3 appears, the host needs a new frontend module or a protocol-translation shim. The isolated-layer design makes this possible without touching the engine or core.

## Related

- [ADR-0003: Session controller — reduce + diff](0003-session-controller-reduce-diff.md) — the state model around the protocol events
