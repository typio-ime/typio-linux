# Security Policy

## Reporting a vulnerability

Report suspected vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/typio-ime/typio-linux/security/advisories/new).
Do not open a public issue for anything that could expose user input.

An input method host observes every keystroke, so treat the following as
security-sensitive even when they look like ordinary bugs:

- Memory corruption reachable from the UDS control socket.
- Key or preedit text appearing in logs, status output, or D-Bus.
- Engine processes receiving input outside an active session.

## Supported versions

Only the latest release receives security fixes.

## Model

The trust boundaries and assumptions are documented in
[Security Model](docs/explanation/security-model.md).
