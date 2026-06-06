# ADR-0028: Direct IPC Engine Workers

- **Status**: Accepted
- **Date**: 2026-06-06
- **Deciders**: Typio maintainers

## Context

ADR-0027 moved engine discovery from in-process shared libraries to IPC
manifests, but it still allowed existing ABI engines to run through a generic
`typio-engine-worker` bridge. That kept two engine implementation models in the
system: native worker executables and shared libraries loaded by a helper.

Maintaining both models makes package layout, diagnostics, test coverage, and
failure semantics ambiguous. Engine authors also need one clear contract.

## Decision

The Linux host discovers only `typio-engine-*.toml` manifest files. Each
manifest declares engine metadata, capabilities, and the command line for one
direct worker executable owned by that engine package.

The host never `dlopen`s engine libraries and no longer ships a generic
`typio-engine-worker` bridge. Rime, Mozc, Sherpa-ONNX, and Basic each build
their own worker executable and install a manifest that points at that
executable.

## Alternatives considered

- **Keep the generic ABI bridge**: Rejected because it preserves a second
  implementation path and makes "native" versus "bridged" worker behavior
  observable in packaging and debugging.
- **Return to in-process plugins**: Rejected because an engine crash or unsafe
  native-code fault would terminate or corrupt the daemon.
- **Support both `.engine` and `.toml` manifests**: Rejected because this is a
  breaking architecture cleanup and compatibility would leave two naming
  contracts in circulation.

## Consequences

- Positive: Every engine package has the same runtime shape: manifest plus
  worker executable.
- Positive: Engine crashes terminate only the worker process; the daemon keeps
  the engine boundary at process level.
- Trade-off: Engine repositories must include their worker entry point instead
  of relying on a shared bridge binary.
- Negative (accepted): Existing shared-library-only engine packages must be
  rebuilt as worker executables.
