# Interface Stability Reference

This page declares the stability tier of every externally consumable
interface. The project follows [Semantic Versioning](https://semver.org/);
while the version is below 1.0.0, the tier definitions below replace the
usual major-version rules.

## Tiers

| Tier | Promise while the project is pre-1.0 |
|------|--------------------------------------|
| **Stable** | Breaking changes require a minor version bump, a `CHANGELOG.md` entry under **Changed**, and a migration note in the affected reference page. |
| **Unstable** | May break in any release. Every breaking change gets a `CHANGELOG.md` entry. No migration support. |
| **Experimental** | May break in any release without notice. Consumers should pin an exact daemon version. |

At 1.0.0, every interface listed as Stable falls under standard SemVer:
breaking changes require a major version bump.

## Interfaces

| Interface | Tier | Consumers | Reference |
|-----------|------|-----------|-----------|
| TIP v1 wire format (UDS framing, JSON-RPC envelope, `protocolVersion` handshake) | Stable | `typioctl`, third-party tools | [IPC Protocol Reference](ipc-protocol.md) |
| TIP method surface (`engine.*`, `keyboard.*`, `voice.*`, `config.*`, `daemon.*`, `events.*`) | Unstable | `typioctl`, third-party tools | [IPC Protocol Reference](ipc-protocol.md) |
| `core.toml` / `platform.toml` keys | Unstable | End users | [Configuration Reference](configuration.md) |
| Engine manifest format (`typio-engine-*.toml`) | Experimental | Engine package authors | [Engine Discovery Reference](engine-discovery.md) |
| Typio Engine Protocol (fd 3 channel) | Experimental, defined by libtypio | Engine package authors | libtypio documentation |
| CLI flags (`typio`, `typioctl`) | Unstable | End users, scripts | [CLI Reference](cli.md) |
| D-Bus status interface and StatusNotifierItem | Experimental | Status bars, desktop shells | — |
| systemd user unit name (`typio.service`) | Stable | Packagers, session managers | [How to Package for Distribution](../how-to/package-for-distribution.md) |

## Versioned negotiation points

| Mechanism | Location | Current value |
|-----------|----------|---------------|
| `protocolVersion` | TIP `hello` response | `2` |
| `protocol` | Engine manifest | `typio-engine-protocol` |

Clients must check `protocolVersion` before using namespace verbs.
Manifests with an unrecognized `protocol` value are skipped at discovery
time.

## Changing a tier

Raising an interface to a higher tier (or breaking a Stable interface)
requires an ADR. Lowering a tier is not permitted; deprecate and replace
the interface instead.
