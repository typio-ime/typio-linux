# Engine Discovery Reference

## Search Path

| Order | Source | Path |
|---|---|---|
| 1 | `-E` / `--engine-dir DIR` | Directories given on the command line; repeatable; scanned in the order given |
| 2 | `$TYPIO_ENGINE_PATH` | Colon-separated directory list; scanned in listed order |
| 3 | System engine dir | Compile-time `<prefix>/<datadir>/typio/engines` |

| Rule | Value |
|---|---|
| Duplicate names | First registered engine wins; later duplicates are rejected |
| Missing directory | Ignored |
| User auto-scan | None |
| Decision record | [ADR-0029](../adr/0029-engine-package-install-layout.md) |

## Manifest Files

| Rule | Value |
|---|---|
| Required prefix | `typio-engine-` |
| Required suffix | `.toml` |
| Loaded example | `typio-engine-rime.toml` |
| Ignored | Files not matching `typio-engine-*.toml` |

## Manifest Keys

| Key | Required | Repeatable | Value |
|---|---:|---:|---|
| `name` | Yes | No | Engine identifier used by config, the command-line interface, and `typioctl` |
| `type` | Yes | No | `keyboard` or `voice` |
| `command` | Yes | No | Worker executable; values containing `/` resolve relative to the manifest file |
| `args` | No | No | Worker argv array; values containing `/` resolve relative to the manifest file |
| `display_name` | No | No | Human-readable name |
| `description` | No | No | Short description |
| `author` | No | No | Engine author or vendor |
| `icon` | No | No | Freedesktop icon name |
| `language` | No | No | BCP-47 language tag; default `und` |
| `required` | No | No | Required capability array |
| `optional` | No | No | Optional capability array |

## Worker Protocol

| Item | Value |
|---|---|
| Transport | Child process stdin/stdout |
| Request format | One tab-separated line |
| Response format | Zero or more response lines ending with `END` |
| Host registration | `typio_registry_register_ipc_engine` |
| Worker model | One executable per engine package |
| Installed worker location | `<prefix>/<libexecdir>/typio/engines/` |
| Installed manifest location | `<prefix>/<datadir>/typio/engines/` |

## Capabilities

| Capability | Host support |
|---|---|
| `preedit` | Yes |
| `candidates` | Yes |
| `prediction` | Yes |
| `punctuation` | Yes |
| `learning` | Yes |
| `voice_input` | Voice builds only |
| `continuous_voice` | Voice builds only |

## Bundled Icons

| Item | Value |
|---|---|
| Location | `<engine-dir>/icons/` |
| Layout | Freedesktop hicolor icon-theme layout |
| Effect | Directory is added to the tray `IconThemePath` |

## Related

- [How to Package for Distribution](../how-to/package-for-distribution.md)
- [Troubleshooting](../how-to/troubleshooting.md)
- [Developer Setup](../dev/setup.md#engine-discovery)
