# ADR-0001: Record Architecture Decisions

- **Status**: Accepted
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

`typio-wayland` is the Wayland host binary (`typiod`) for the Typio input method: it owns the `zwp_input_method_v2` protocol surface, the event loop, the candidate-popup GPU pipeline, the D-Bus adapter, and the platform plugin loader. The framework core lives in `libtypio` and is platform-neutral; this repository contains everything Wayland- and Vulkan-shaped that the daemon does on top of it. As the host grows, design choices accumulate that future contributors must understand.

## Decision

This project uses Architecture Decision Records (ADRs) stored in `docs/adr/`.

- Each ADR is numbered sequentially and is append-only after acceptance.
- To change a past decision, write a new ADR that supersedes the old one and update the old ADR's status field only.
- ADRs are short (ideally one page) and focus on context, decision, alternatives, and consequences.

## Alternatives considered

- **Inline design comments in code**: Rejected. Comments describe *what* the code does, not *why* a larger design choice was made.
- **Long-form architecture documents only**: Rejected. Explanation docs are mutable; ADRs provide an immutable anchor.

## Consequences

- Positive: new contributors understand why key boundaries exist without reading the entire commit history.
- Positive: reviewers can require an ADR for architectural changes.
- Trade-off: maintainers must remember to write ADRs for significant decisions.
