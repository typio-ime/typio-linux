# ADR-0007: D-Bus Adapter as a Thin Transport over `TypioStatusService`

- **Status**: Superseded by [ADR-0008](0008-ipc-protocol-resource-namespaces-uds-only.md)
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

The IPC refactor replaced D-Bus with UDS for the primary control channel and introduced `TypioStatusService` as a transport-agnostic business-logic layer. Initially the D-Bus adapter retained its own full implementation of every method handler (ActivateEngine, NextEngine, ReloadConfig, …), duplicating the logic in `status_service.c`.

That arrangement created two parallel sets of business logic:

1. `TypioStatusService` — used by the UDS server.
2. `TypioStatusBus` (D-Bus) — used by the D-Bus adapter.

Any change to state semantics (e.g. saving config after engine activation) had to be made in both places, risking subtle behavioural differences between the UDS and D-Bus paths.

## Decision

Make the D-Bus adapter a **thin transport layer** that delegates all business logic to `TypioStatusService`.

- `TypioStatusBus` owns a `TypioStatusService *service` instance.
- D-Bus method handlers extract arguments from `DBusMessage`, marshal them into a JSON `params` object, call `typio_status_service_handle()`, and convert the JSON response back into a D-Bus reply.
- Get / GetAll property handlers remain unchanged (they only read state and already share query paths via `append_property_variant`).
- State-controller bindings and runtime-state callbacks are forwarded to the underlying `TypioStatusService` so both transports share identical notification paths.

## Alternatives considered

- **Keep dual logic paths.** Rejected: violates DRY and guarantees divergence over time.
- **Move D-Bus into `TypioIpcBus`.** Rejected: the D-Bus adapter is an optional compile-time feature (`HAVE_STATUS_BUS`); coupling it to the always-on UDS bus would complicate conditional compilation and increase the daemon's dependency surface.

## Consequences

- Positive: single source of truth for all control / state operations.
- Positive: bug fixes or behavioural changes apply to both transports automatically.
- Trade-off: D-Bus handlers perform an extra JSON marshal/unmarshal round-trip. Negligible at control-plane message volume.
- Negative (accepted): the D-Bus adapter now depends on the JSON / IPC-protocol helpers for response parsing, slightly increasing internal coupling.
