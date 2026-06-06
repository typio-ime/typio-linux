# IPC Protocol Reference (TIP v1)

The Typio daemon exposes a **Unix Domain Socket** carrying length-prefixed JSON-RPC 2.0. This is the only control transport (ADR-0008).

## Socket location

| Variable | Default |
|---|---|
| Path | `$XDG_RUNTIME_DIR/typio/daemon.sock` (fallback: `~/.local/share/typio/daemon.sock`) |
| Mode | `0600`; peer uid must match the daemon uid (`SO_PEERCRED`). |

## Wire format

```
[ 4 bytes: payload length in bytes (big-endian uint32) ]
[ N bytes: UTF-8 JSON payload                          ]
```

Used identically for requests, responses, and server→client event notifications. Max frame size: 1 MiB.

## JSON conventions

- Method names: dotted `namespace.action` (e.g. `engine.list`).
- All object keys and string values: **camelCase**.
- Numbers are JSON numbers; the `type` field of a config value distinguishes `string` / `int` / `bool` / `float`.

## Request

```json
{ "jsonrpc": "2.0", "id": <int>, "method": "<dotted>", "params": <object> }
```

`params` is omitted when the method takes none.

## Response

Success:
```json
{ "jsonrpc": "2.0", "id": <int>, "result": <value> }
```

Error:
```json
{ "jsonrpc": "2.0", "id": <int>, "error": { "code": <int>, "message": "<str>" } }
```

Error codes follow the JSON-RPC 2.0 reserved range:

| Code | Meaning |
|---|---|
| `-32700` | Parse error |
| `-32600` | Invalid request |
| `-32601` | Method not found / not supported by target |
| `-32602` | Invalid params (unknown key/engine/etc.) |
| `-32603` | Internal error |

## Notification (server → client)

```json
{ "jsonrpc": "2.0", "method": "<topic>", "params": <payload> }
```

Notifications have no `id` and expect no reply. The client must have subscribed via `events.subscribe`; unsubscribed clients receive nothing.

## Methods

### `hello`

| Direction | params | result |
|---|---|---|
| C → S | `{}` | `{ protocolVersion, daemonVersion, capabilities: [string...] }` |

`protocolVersion` is an integer (`2` in this release; v2 replaced `engine.use` / `engine.next` with the modality-explicit `keyboard.*` / `voice.*` verbs — ADR-0026). `capabilities` enumerates the top-level namespaces the daemon supports — currently `["config", "engine", "keyboard", "voice", "daemon", "events"]`.

### `config.*`

| Method | params | result |
|---|---|---|
| `config.get` | `{ key }` | `{ value, type, source }` (`source` is `"user"` or `"default"`) |
| `config.set` | `{ key, value }` | `{}` |
| `config.unset` | `{ key }` | `{}` |
| `config.list` | `{ prefix? }` | `[{ key, type, value, label, section, choices? }, ...]` |
| `config.show` | `{}` | `{ text, format: "toml" }` |
| `config.reload` | `{}` | `{}` |

`key` is a dotted path against the unified config tree. `value` is always a string in `config.set`; the daemon coerces using the schema's typed field. For an engine-namespaced key (`engines.<name>.<key>`) the daemon also delivers `on_config_change` to the owning engine (libtypio ADR-0008).

### `engine.*` / `keyboard.*` / `voice.*`

The `engine.*` namespace is cross-modality and keyed by engine name (aggregate query + lifecycle). Activation and cycling are modality-explicit (ADR-0026): the keyboard and voice slots are orthogonal and simultaneously active, so each has its own verbs under `keyboard.*` / `voice.*`.

| Method | params | result |
|---|---|---|
| `engine.list` | `{}` | `[{ name, kind, displayName, active }, ...]` |
| `engine.describe` | `{ name }` | `{ name, kind, displayName, properties: [...], commands: [...] }` |
| `engine.invoke` | `{ name, command, args? }` | `{}` |
| `engine.load` | `{ path }` | `{ loaded, path }` |
| `engine.unload` | `{ name }` | `{ unloaded, name }` |
| `engine.reload` | `{ name, path? }` | `{ reloaded, name, path? }` |
| `keyboard.use` | `{ name }` | `{}` |
| `keyboard.next` | `{}` | `{ active }` |
| `keyboard.prev` | `{}` | `{ active }` |
| `voice.use` | `{ name }` | `{}` |
| `voice.next` | `{}` | `{ active }` |
| `voice.prev` | `{}` | `{ active }` |

`kind` (in `engine.list` / `engine.describe`) is `"keyboard"` or `"voice"`. `keyboard.use` / `voice.use` reject a `name` whose engine is not of the matching modality. Each property entry in `engine.describe` carries `{ key, type, value, label, choices? }`.

`engine.load` loads a single engine manifest from an absolute `.toml` path. `engine.unload` unregisters an engine by name (deactivating it first if active). `engine.reload` combines unload + load: if `path` is provided, loads from that exact path; if omitted, rescans the configured `engine_dirs` to find the engine by name (`typio-engine-<name>.toml`).

### `daemon.*`

| Method | params | result |
|---|---|---|
| `daemon.status` | `{}` | `{ version, protocolVersion, uptimeSeconds, activeKeyboardEngine, activeVoiceEngine, runtime? }` |
| `daemon.version` | `{}` | `{ version }` |
| `daemon.stop` | `{}` | `{}` |

`runtime` is present when the runtime-state callback is wired (Wayland host); see `daemon.status` schema below.

### `events.subscribe`

| Method | params | result |
|---|---|---|
| `events.subscribe` | `{ topics?: [string...] }` | `{ subscribed: true }` |

Subscribes the calling connection to one or more topics. Omitting `topics` (or sending `[]`) subscribes to every topic. The subscription persists for the lifetime of the connection.

## Event topics

| Topic | Payload |
|---|---|
| `engine.changed` | `{ activeKeyboardEngine, activeVoiceEngine }` |
| `engine.statusChanged` | `{ modeId, modeLabel, displayLabel, iconName, profileId, profileLabel }` |
| `config.changed` | (reserved — emitted on config writes; payload TBD) |
| `runtime.changed` | (reserved — emitted on runtime-state edges) |
| `daemon.shuttingDown` | `{}` |

## `daemon.status` schema

| Field | Type |
|---|---|
| `version` | string |
| `protocolVersion` | int |
| `uptimeSeconds` | int |
| `activeKeyboardEngine` | string (empty if none) |
| `activeVoiceEngine` | string (empty if none) |
| `runtime.frontendBackend` | string |
| `runtime.lifecyclePhase` | string |
| `runtime.virtualKeyboardState` | string |
| `runtime.keyboardGrabActive` | bool |
| `runtime.watchdogArmed` | bool |
