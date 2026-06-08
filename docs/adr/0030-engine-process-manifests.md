# ADR-0030: Engine process manifests and Typio Engine Protocol

- **Status**: Accepted
- **Date**: 2026-06-08
- **Deciders**: Typio maintainers
- **Supersedes**: [ADR-0027](0027-ipc-engine-manifests.md), [ADR-0028](0028-direct-ipc-engine-workers.md)
- **Amends**: [ADR-0029](0029-engine-package-install-layout.md)

## Context

ADR-0027 and ADR-0028 moved Linux engine loading from in-process shared
libraries to manifest-declared executable processes. That decision was correct,
but the public terminology remained "IPC worker". The phrase names a transport
technique and an implementation role rather than the contract the host exposes.

libtypio now names the core contract Typio Engine Protocol and names the runtime
lifecycle an engine process. The Linux host must keep its manifest schema,
loader diagnostics, and documentation aligned with that contract.

## Decision

The Linux host discovers `typio-engine-*.toml` files and accepts only manifests
that declare:

```toml
protocol = "typio-engine-protocol"
```

The manifest `command` and `args` fields describe the engine process argv. The
host resolves relative paths against the manifest directory, performs capability
negotiation, and registers the engine with libtypio through
`typio_registry_register_engine_process`.

The host does not use stdin/stdout as the engine protocol transport. libtypio
starts the engine process, passes Typio Engine Protocol on the private fd 3
channel, and reserves stdout/stderr for logs.

## Alternatives considered

- **Keep `IPC worker` in manifests and docs**: Rejected because it preserves the
  same naming ambiguity that the core API removed.
- **Name the manifest value `ipc`**: Rejected because it describes the mechanism,
  not the host/engine contract.
- **Add `v2` to the manifest protocol value**: Rejected because protocol
  versions belong in the frame header and handshake, not in the product name.
- **Keep accepting old protocol values**: Rejected because this is a deliberate
  greenfield cleanup with no backward-compatibility goal.

## Consequences

- Positive: Host docs, manifest schema, loader logs, and libtypio API all use
  the same engine-process terminology.
- Positive: Engine logs cannot corrupt protocol traffic.
- Positive: The host can reject unsupported engine contracts before activation.
- Trade-off: Existing manifests must add or update the `protocol` key.
- Negative (accepted): Old `worker-v2` and `typio-engine-ipc` style manifests
  no longer load.
