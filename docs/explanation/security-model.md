# Security Model

The daemon is an input method host: it observes every keystroke the user
types into Wayland clients and, with voice enabled, captures microphone
audio. That makes it a high-value target, and it makes the trust
boundaries worth stating explicitly. This page records what the daemon
trusts, what it does not, and where the enforcement lives.

## Trust boundaries

The daemon runs as an unprivileged process inside the user session. Its
security model is the user boundary: everything running as the same uid
is trusted, everything else is excluded by the kernel before it reaches
the daemon.

| Surface | Exposure | Enforcement |
|---------|----------|-------------|
| UDS control socket (TIP) | Same-uid processes | Socket mode `0600`; `SO_PEERCRED` uid check on accept (`src/ipc/uds_server.c`) |
| TIP frames | Arbitrary bytes from same-uid clients | 4-byte length prefix, 1 MiB frame cap, hand-written JSON parser (`src/ipc/tip_json.c`) |
| Engine worker processes | Full user privileges | None at runtime — trusted by installation (see below) |
| Wayland protocols | Compositor | Compositor is fully trusted; it grants the input-method role |
| D-Bus session bus, StatusNotifierItem | Same-session peers | Status output only; no privileged verbs |
| PipeWire capture | Audio session | Standard PipeWire client; subject to session policy |

A same-uid attacker already controls the user account, so the UDS and
D-Bus surfaces defend integrity of the daemon process (no memory
corruption from malformed frames), not confidentiality from peers. The
TIP JSON parser is the one component that parses externally supplied
bytes, which is why it is the project's fuzzing target.

## Engines are trusted by installation

An engine is an arbitrary executable named by a manifest. The daemon
spawns it with the user's full privileges and streams every keystroke of
the active session to it over the fd 3 Typio Engine Protocol channel.
There is no runtime confinement: **installing an engine manifest is
equivalent to installing a keylogger**, and the
[engine discovery search path](../reference/engine-discovery.md)
is the entire installation-time control. The daemon never auto-scans
user-writable locations; engines come from the compile-time system
directory, an explicit `--engine-dir`, or an explicit
`$TYPIO_ENGINE_PATH`.

This is the current assumption, not the end state. The process model was
chosen partly because it leaves a sandboxing path open
([ADR-0028](../adr/0028-direct-ipc-engine-workers.md)): each engine is a
separate child with a single private fd for protocol traffic and
stdout/stderr reserved for logs, so per-engine confinement (Landlock,
seccomp, or systemd properties such as `NoNewPrivileges=` and
`RestrictNamespaces=`) can be added in the spawner without touching the
architecture. Engines need file access for their dictionaries and models,
so confinement policy will likely be manifest-declared rather than fixed.

## What the daemon does not defend against

- A compromised user account. Any same-uid process can read keystrokes
  by other means (for example by talking to the compositor); the daemon
  does not attempt to be more private than the session it runs in.
- A malicious compositor. The input-method grant comes from the
  compositor; there is no way to operate below it.
- Malicious engines, today. See above.

## Reporting

Report suspected vulnerabilities as described in the repository's
[security policy](../../SECURITY.md).
