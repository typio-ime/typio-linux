# ADR-0029: Engine Package Install Layout

- **Status**: Accepted
- **Date**: 2026-06-06
- **Deciders**: Typio maintainers

## Context

ADR-0028 made every engine package a direct IPC worker. These workers are not
user-facing applications: running one directly only speaks the worker protocol
over stdin/stdout. They do not attach to Wayland or commit text without the
Linux host.

The previous manifest location, `<libdir>/typio/engines`, came from the
shared-library plugin model. After the IPC worker change, manifests are
metadata and worker executables are private helpers.

## Decision

Engine packages install worker executables under
`<prefix>/<libexecdir>/typio/engines/`.

Engine packages install manifests under
`<prefix>/<datadir>/typio/engines/`.

Installed manifests use an absolute `command` path pointing at the installed
worker under `libexecdir`. Development manifests generated in build trees may
use `./typio-engine-*` so `--engine-dir build` continues to work without
installation.

Engine icons stay under the freedesktop icon theme layout,
`<prefix>/<datadir>/icons/hicolor/...`.

## Alternatives considered

- **Install workers in `<bindir>`**: Rejected because engine workers are not
  user commands and do not produce useful behavior when run directly.
- **Install manifests in `<libdir>`**: Rejected because manifests are
  architecture-independent metadata, not libraries or private executables.
- **Put workers next to manifests**: Rejected because it mixes executable
  helpers into a data directory.
- **Rely on `PATH` for installed workers**: Rejected because systemd user
  service environments may have a minimal or distribution-specific `PATH`.

## Consequences

- Positive: Package layouts match Unix filesystem intent: user commands in
  `bindir`, private helpers in `libexecdir`, metadata in `datadir`.
- Positive: Installed manifests do not depend on the daemon process `PATH`.
- Trade-off: Engine build systems generate separate development and installed
  manifests.
- Negative (accepted): Existing packages that placed manifests under `libdir`
  must move them to `datadir`.
