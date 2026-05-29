# ADR-0008: TIP v1 — IPC Protocol with resource namespaces, UDS-only control surface, push events

- **Status**: Accepted
- **Date**: 2026-05-29
- **Deciders**: Project maintainers

## Context

The pre-v2 control IPC accumulated several kinds of debt:

1. **Two parallel transports for the same control surface.** UDS (JSON-RPC) for `typioctl` and D-Bus (`org.typio.InputMethod1`) for `typio-settings`. ADR-0007 tried to deduplicate by making the D-Bus adapter "thin", but `state/dbus.c` (1143 lines) still carries its own property-name table, GetAll list, and per-property dispatch — adding a property meant updating two places.
2. **Duplicate property names per transport.** Both `OrderedKeyboardEngines` and `OrderedEngines` are surfaced; both `ActiveKeyboardEngine` and `ActiveEngine`. The duplication was left mid-rename and never finished.
3. **`SetRimeSchema` and friends as first-class IPC methods.** Engine-specific calls (`SetRimeSchema`, `RimeSchema` property, the implicit-active-engine `InvokeEngineCommand`) exist only because the generic engine surface did not work. With libtypio ADR-0008 (engine properties unified into the config schema), that excuse is gone.
4. **`SetConfigText` as the only config-write surface.** Replaces the entire daemon config blob with each call — every client must round-trip the whole file.
5. **Polling-only client state.** Clients call `GetAll` on a timer because there is no push channel. The settings panel polls; the CLI re-queries; the wayland-aux notifier listens to `PropertiesChanged` but it only fires for state-controller events, not for arbitrary config writes.
6. **PascalCase methods and property keys.** Mixes uncomfortably with JSON convention (camelCase) and with libtypio's snake-case dotted config keys (`engines.rime.schema`).
7. **No protocol version handshake.** Clients can't probe what the daemon supports.

## Decision

Replace the IPC contract with a single, resource-oriented, camelCase JSON-RPC protocol over UDS only. Drop the D-Bus control interface and every legacy method.

### Transport

- **UDS only.** `$XDG_RUNTIME_DIR/typio/daemon.sock`, 4-byte big-endian length prefix + JSON-RPC 2.0 body — unchanged framing. The D-Bus dependency in `typio-wayland` is retained for `StatusNotifierItem` (systray), but the `org.typio.InputMethod1` control interface and `state/dbus.c`, `state/dbus.h`, `ipc/dbus_protocol.h` are removed.
- **Connection model.** UDS connections become long-lived; clients may issue any number of request/response method calls and, optionally, subscribe to event streams over the same connection.

### Methods

Three resource namespaces plus a connection-level `hello` and an `events.subscribe`. All method names are dotted camelCase strings.

```
hello {}
  -> { protocolVersion, daemonVersion, capabilities: [...] }

config.get      { key }                  -> { value, type, source }
config.set      { key, value }           -> {}
config.unset    { key }                  -> {}
config.list     { prefix? }              -> [{ key, value, type, schema }]
config.show     {}                       -> { text, format }
config.reload   {}                       -> {}

engine.list     {}                       -> [{ name, kind, displayName, active }]
engine.describe { name }                 -> { name, kind, displayName, properties: [...], commands: [...] }
engine.use      { name }                 -> {}
engine.next     { kind? }                -> { active }
engine.invoke   { name, command, args? } -> { result }

daemon.status   {}                       -> { version, uptime, activeKeyboardEngine, activeVoiceEngine, ... }
daemon.stop     {}                       -> {}
daemon.version  {}                       -> { version }

events.subscribe { topics?: [...] }      -> stream of { topic, payload }
```

### Events

Push-based. Topic strings are dotted: `engine.changed`, `engine.mode_changed`, `config.changed`, `daemon.shutting_down`, `runtime.state_changed`. `events.subscribe` with no topics subscribes to everything. The reply opens a stream; each subsequent server-initiated message on the connection is a `{ topic, payload }` notification framed identically to a JSON-RPC notification.

### Config keys

Dotted keys against the unified config tree (libtypio's `TypioConfig` + schema layer). `engine.set <name> <key> <value>` in the CLI maps to `config.set { key: "engines.<name>.<key>", value }`; the host then calls `typio_registry_notify_config_change` so the engine reacts via `on_config_change` (libtypio ADR-0008).

### Removed surface

- `dispatch_property` and the `TYPIO_IPC_PROP_*` / `TYPIO_STATUS_PROP_*` tables.
- Method handlers: `GetAll`, `Get`, `ActivateEngine`, `NextEngine`, `SetEngineProperty`, `InvokeEngineCommand`, `SetConfigText`, `ReloadConfig`, `Stop`.
- Properties: `ActiveKeyboardEngine`, `ActiveEngine`, `AvailableKeyboardEngines`, `AvailableEngines`, `OrderedKeyboardEngines`, `OrderedEngines`, `EngineDisplayNames`, `EngineOrder`, `AvailableVoiceEngines`, `ActiveVoiceEngine`, `ActiveEngineState`, `ActiveEngineMode`, `RuntimeState`, `ActiveEngineProperties`, `ActiveEngineCommands`, `ConfigText`.
- D-Bus control interface and the entire `state/dbus.*` + `ipc/dbus_protocol.h` chain.
- `enable_status_bus` build option (the systray uses its own D-Bus client; it does not depend on the control adapter).

### Migration

There are no shims and no deprecation aliases. `typioctl` (Phase D) and `typio-settings` (Phase E) port to the new vocabulary in the same wave.

## Alternatives considered

- **Keep both UDS and D-Bus surfaces; finish the camelCase rename in both.** Rejected: ADR-0007 already tried to reconcile the two and the duplication crept back in. Maintaining a second transport surface for the same RPCs is the textbook two-sources-of-truth bug pattern. D-Bus loses the daemon-control role; it stays only where it adds something irreplaceable (StatusNotifierItem).

- **Keep flat method names (`ConfigGet`, `EngineList`) rather than dotted namespaces.** Rejected: dotted methods read better in handshakes, group cleanly in client SDKs, and align with the dotted config keys we already use. The namespaces are an asset on day one.

- **Make events.subscribe a separate transport (named pipe or second socket).** Rejected: one connection handles both directions cleanly with the existing 4-byte-length framing. Two transports = two failure modes.

- **Versioned legacy methods kept for one release as deprecated aliases.** Rejected per project mandate (greenfield rewrite, no backward compatibility). In-tree callers (`typioctl`, `typio-settings`) move in the same wave.

- **Add an `events.poll` HTTP-long-poll-style fallback for clients that can't hold a connection.** Rejected: every in-tree client can hold a UDS connection. If a future browser client needs long-poll the bridge belongs in an external proxy.

## Consequences

- Positive: one source of truth for every control RPC; one transport for the control surface.
- Positive: clients stop polling. Settings panel reflects changes within one socket round-trip of any engine switch, config write, or shutdown.
- Positive: protocol version handshake unlocks forward-compatible client behaviour.
- Positive: ~1200 lines deleted across `state/dbus.c`, `dbus.h`, and `ipc/dbus_protocol.h`. The IPC catalog shrinks from 17 properties + 9 methods to 3 namespaces + 1 events channel.
- Trade-off: third-party D-Bus consumers of `org.typio.InputMethod1` stop working. There are no known third-party consumers; in-tree consumers are `typioctl` (UDS already) and `typio-settings` (ported in Phase E).
- Trade-off: long-lived UDS connections need a per-client subscription table in `uds_server.c` (a few hundred bytes per connected client). Negligible given the 16-client connection cap.
- Negative (accepted): an external script that previously did `gdbus call --session --dest org.typio.InputMethod1 --object-path /org/typio/InputMethod1 --method org.typio.InputMethod1.ActivateEngine 'rime'` now needs to speak UDS JSON-RPC. The repository ships a CLI (`typioctl`) for exactly this purpose.

## Related

- [ADR-0007: D-Bus adapter as a thin transport over `TypioStatusService`](0007-dbus-adapter-over-status-service.md) — superseded for the control surface (D-Bus removed); the adapter's "thin shim over service" principle survives as a UDS-only single source of truth.
- libtypio ADR-0007: IPC ownership — control surface to host, engine backend deferred — establishes that this contract lives in `typio-wayland`.
- libtypio ADR-0008: engine properties unified into the config schema layer — makes the generic engine config surface usable so engine-specific methods can be removed.
