# ADR-0027: IPC Engine Manifests

- **Status**: Accepted
- **Date**: 2026-06-05
- **Deciders**: Typio maintainers

## Context

Typio engines run keystroke-privileged code. Loading arbitrary engine libraries into the daemon keeps engine faults and malicious behavior in the host process. The previous discovery model also coupled the host to shared-library filenames instead of an explicit engine contract.

## Decision

The Linux host discovers only `typio-engine-*.engine` manifest files. A manifest declares engine metadata, required and optional capabilities, a worker command, and worker arguments. The host registers the engine with libtypio through `typio_registry_register_ipc_engine`.

The daemon never `dlopen`s engine libraries. Existing ABI engines run through the separate `typio-engine-worker` helper process, which may load an engine `.so` inside the helper and translate the C ABI to the line-oriented IPC worker protocol.

## Alternatives considered

- **In-process plugins**: Rejected because engine crashes and unsafe native code faults terminate or corrupt the daemon.
- **Hybrid plugin and IPC loading**: Rejected because maintaining two loading models makes engine packaging, tests, and failure semantics ambiguous.
- **Host-side direct `.so` discovery with worker wrapping**: Rejected because the host would still define engines by binary filename instead of an explicit manifest.

## Consequences

- Positive: Engine crashes are isolated from the daemon process.
- Positive: Engine packages declare metadata and capabilities before execution.
- Trade-off: Every engine call crosses a process boundary.
- Trade-off: Existing ABI engines need `typio-engine-worker` or a native IPC worker executable.
- Negative (accepted): Engine packages without manifests are no longer discovered.
