# Engine Discovery Reference

How `typio` locates and loads engine plugins at startup.

## Search path (priority order)

| Order | Source | Path |
|---|---|---|
| 1 | `-E` / `--engine-dir DIR` | directory given on the command line |
| 2 | `$TYPIO_ENGINE_DIR` | value of the environment variable |
| 3 | User lib dir | `~/.local/lib/typio/engines` |
| 4 | System lib dir | compile-time `<prefix>/<libdir>/typio/engines` (e.g. `/usr/lib/typio/engines`, or `/usr/local/lib/typio/engines` for a `/usr/local` prefix) |

- All existing directories in the list are scanned, in the order above.
- The **first** engine of a given `<name>` registers; a later duplicate is rejected (`AlreadyExists`).
- Consequence: a user-directory engine **shadows** a system engine of the same name.

## File name convention

| Rule | Value |
|---|---|
| Required prefix | `libtypio_engine_` |
| Required suffix | `.so` |
| Engine identifier | `<name>` — the text between prefix and suffix |
| Loaded example | `libtypio_engine_basic.so` → identifier `basic` |
| Ignored | any file not matching `libtypio_engine_*.so` (silently skipped) |

- Cargo emits `libtypio_engine_<name>.so` natively — no rename needed.
- `<name>` is the identifier used in config keys (`engines.<name>.*`), the `--engine` flag, and `typioctl`.

## Bundled icons (optional)

| Item | Value |
|---|---|
| Location | `<engine-dir>/icons/` (freedesktop hicolor layout) |
| Effect | the directory is added to the tray's `IconThemePath` |
| Resolves | `TypioEngineInfo.icon` and the engine's status `icon_name` |

## Related

- [How to Package for Distribution](../how-to/package-for-distribution.md) — install paths for packagers
- [Troubleshooting: no engines](../how-to/troubleshooting.md) — common discovery failures
- [Developer Setup](../dev/setup.md#engine-discovery) — building and installing an engine locally
