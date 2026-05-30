# Typio Wayland Documentation

The Linux/Wayland host for the [Typio](../typio) input method framework. This repository contains the `typio` binary and its supporting code: the Wayland input-method frontend, candidate popup renderer, UDS control surface (TIP v1), system tray, and voice capture plumbing.

## Sections

- **[How-to Guides](how-to/)** — Task-oriented recipes for specific goals.
  - [How to Package for Distribution](how-to/package-for-distribution.md)
  - [Troubleshooting](how-to/troubleshooting.md)
- **[Reference](reference/)** — Lookup-oriented API, config, and protocol documentation.
  - [IPC Protocol Reference](reference/ipc-protocol.md) — TIP v1 (UDS + JSON-RPC)
  - [Configuration Reference](reference/configuration.md)
  - [Engine Discovery Reference](reference/engine-discovery.md) — search path, file-name rules, icons
- **[Explanation](explanation/)** — Understanding-oriented design documents.
  - [Wayland Input Method Protocol](explanation/wayland-input-method.md)
  - [Frontend Graphics](explanation/frontend-graphics.md)
  - [Timing Model](explanation/timing-model.md)
  - [Lifecycle Resilience and Recovery](explanation/lifecycle-resilience.md)
  - [Control Surfaces](explanation/control-surfaces.md)
- **[Developer Documentation](dev/)** — Contributor-oriented docs.
  - [Developer Setup](dev/setup.md)
  - [Testing](dev/testing.md)
  - [Code Style](dev/code-style.md)
  - [Popup Appearance](dev/popup-appearance.md)
  - [Maintenance Manual](dev/maintenance.md)

## Quick Links

- [README](../README.md) — Project pitch and quick start
- [Contributing](../CONTRIBUTING.md) — How to contribute
