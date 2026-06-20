# Agent guidelines

This file is the entry point for any AI agent (Claude, GPT, local LLM, etc.)
working in this repository. Read it before touching anything. It captures
project-specific conventions that are NOT derivable from the code alone and
that the agent will otherwise have to rediscover by breaking things.

The existing single-line note still applies:

> Before writing, modifying, or archiving any documentation, please read and
> follow `docs/dev/documentation-style-guide.md` file in the project root.

## 1. Pre-flight (run every session before editing)

```bash
# Recent commit message style — match it
git --no-pager log --oneline -15

# Tag type the project uses (annotated vs lightweight)
git cat-file -t "$(git describe --abbrev=0)"

# Current version source
grep -n "version:" meson.build | head -1

# Pending CHANGELOG entries
sed -n '/^## \[Unreleased\]/,/^## \[/p' CHANGELOG.md
```

If any of these surprise you, STOP and reconcile with what the agent planned
to do. Do not assume generic defaults.

## 2. Commit message conventions

The repo uses Conventional-Commits-style prefixes observed in history:

| Prefix | Use |
|---|---|
| `feat:` | New user-visible feature |
| `fix:` or `fix(scope):` | Bug fix (scope optional: `fix(panel):`, `fix(tray):`) |
| `test:` or `test(scope):` | Test-only change |
| `docs:` | Documentation only |
| `build:` | Build system, wraps, deps |
| `ci:` | CI config |
| `release: vX.Y.Z` | The release commit (see §4) |
| `subsystem:` (e.g. `tray:`, `shortcut:`) | Looser form also seen; pick the conventional prefix when in doubt |

Rules:
- Subject ≤ ~70 chars, lowercase, no trailing period.
- Body wraps at ~72 cols, explains **why** not **what**.
- Reference ADRs by ID when applicable (`(ADR-0034)`).
- **Never add `Co-Authored-By:` or any agent attribution.** This is the
  user's hard rule (see `~/.claude/CLAUDE.md`).

Always commit with `-m "subject"` and (if needed) `-m "body"` — never let an
editor launch. Interactive editors in agent-driven shells tend to hang the
tool runner.

## 3. Tag conventions (this is what bit us before)

**All release tags in this repo are annotated**, not lightweight. Verify:

```bash
$ git cat-file -t v0.3.3
tag
```

Creating a release tag MUST use `-a -m` so no `$EDITOR` is invoked:

```bash
git tag -a vX.Y.Z -m "release: vX.Y.Z"
```

Equivalents that ALSO trigger the editor and must NOT be used in agent runs:
- `git tag vX.Y.Z` (lightweight — wrong type for this repo anyway)
- `git tag -a vX.Y.Z` (annotated, but no inline message)
- `git tag -a vX.Y.Z -m ""` (some git versions still open editor on empty msg)

**Diagnostic tip**: if `nvim` / `vim` / `$EDITOR` suddenly launches during a
git operation, git is asking for a MESSAGE (commit, tag, rebase), not paging
output. `--no-pager` and `GIT_PAGER=cat` do NOT help. The fix is to supply
`-m` to the command that wanted the message.

## 4. Release workflow

A release is two commits + one tag, in this order:

1. **Substantive commits first** — feature/fix/test work, each with their own
   conventional prefix. CHANGELOG entries go under `## [Unreleased]` as the
   work lands.
2. **Release commit**: bump `version: 'X.Y.Z'` in `meson.build` AND move the
   `## [Unreleased]` block to `## [X.Y.Z] - YYYY-MM-DD` in `CHANGELOG.md`,
   in a single commit titled `release: vX.Y.Z`. Nothing else in that commit.
3. **Annotated tag** `vX.Y.Z` with message `release: vX.Y.Z` (see §3).
4. `git push origin main && git push origin vX.Y.Z`.

Version bump rules:
- **patch +1** (0.3.2 → 0.3.3): bug fixes and internal improvements, no
  user-visible API/behavior change.
- **minor +1** (0.3.x → 0.4.0): user-visible new feature or behavior.
- **major +1** (x.y.z → 1.0.0): incompatible change (rare; per SemVer).

Patch releases do NOT need a `## [Unreleased]` placeholder above the new
version block; the next feature commit recreates it.

## 5. Version source

The single source of truth is line 5 of `meson.build`:

```meson
project('typio-linux', 'c',
    version: '0.3.3',
```

Do NOT bump version in `typio_build_config.h.in`, Cargo manifests, or
anywhere else — they derive from `meson.build`.

## 6. CHANGELOG format

[Keep a Changelog](https://keepachangelog.com/) order: `### Added` →
`### Changed` → `### Deprecated` → `### Removed` → `### Fixed` →
`### Security`. Within a section, bullet entries start with a **bold lead
phrase** (`- **Candidate-selection lag...**`) followed by the explanation.
Match the existing voice — terse, technical, explains the user-visible
impact first and the mechanism second.

Date format: `YYYY-MM-DD` (ISO 8601). Get it from `date -I` or
`date +%F` — do NOT hardcode from memory.

## 7. Build, test, and verification gates

Before committing anything non-trivial:

```bash
# Regular build + tests
ninja -C build && meson test -C build

# Sanitizers — CI runs these and they WILL fail the PR
meson setup -Denable_asan=true -Denable_ubsan=true build-asan  # one-time
ninja -C build-asan
LSAN_OPTIONS="suppressions=$(pwd)/tests/asan_suppressions.txt" \
    meson test -C build-asan
```

`LSAN_OPTIONS` is required locally (CI sets it in
`.github/workflows/ci.yml`): without it, LeakSanitizer reports
internal libfontconfig leaks that are already pattern-matched in
`tests/asan_suppressions.txt` but the local `meson test` invocation
doesn't load the file on its own.

ASan leak detection has Fontconfig-internal leaks suppressed in
`tests/asan_suppressions.txt`. If a NEW leak appears, verify it is in
libfontconfig (not our code) before suppressing; otherwise fix it.

CI runs `-Dwerror=true`, so warnings fail the build. Match the existing
`-Wconversion -Wsign-conversion` discipline carefully — implicit
conversions are errors in practice.

## 8. Repo layout and cross-repo work

This is a workspace of sibling projects under `/home/ming/projects/typio/`:

| Path | Role |
|---|---|
| `typio-linux/` | This repo — Wayland host (the C daemon) |
| `libtypio/` | Rust framework library (linked via pkg-config or wrap) |
| `flux/` | Vulkan GPU canvas library (wrap; GPU panel rendering) |
| `typio-engine-*` | Engine plugins (rime, mozc, basic, sherpa, whisper, template) |
| `typio-settings/`, `typioctl/`, `typio-vet/`, `typio-docs/` | Tools and docs |

**Cross-repo changes**: the user has explicitly authorized edits to sibling
repos (e.g. `../flux`) when a fix genuinely belongs there. When touching a
sibling repo:
- Read its own `AGENTS.md` / `CLAUDE.md` first.
- Each repo has its own version, CHANGELOG, and release cadence — do NOT
  bump them in lockstep with typio-linux unless asked.
- Pin updates go in `subprojects/*.wrap` in typio-linux as a separate
  `build:` commit.

## 9. Common agent failure modes (do not repeat)

| Symptom | Wrong reaction | Right reaction |
|---|---|---|
| `$EDITOR` (nvim/vim) launches during git op | Try `--no-pager`, `GIT_PAGER=cat` | git is asking for a message — supply `-m` |
| `-Wconversion` warning | Cast around it | Match the existing signed/unsigned discipline; only cast at boundaries |
| Test fails under ASan | Suppress | Investigate; only suppress if the leak is in a system lib (already-suppressed pattern: fontconfig) |
| `meson test` finds no tests | Use `ctest` | Use `meson test -C build` (this project uses meson, not raw ctest) |
| Can't decide patch vs minor bump | Default to minor "to be safe" | Default to patch — only minor when user-visible behavior changes |
| "Don't know X about the project" | Guess from generic knowledge | Run §1 pre-flight first; project convention beats generic default |

## 10. Documentation governance

`docs/dev/documentation/` has its own routing and style rules. AI agents may
read it and **suggest** changes, but must not directly modify files in that
directory. Other docs (`docs/dev/code-style.md`, `docs/dev/testing.md`,
`README.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `AGENTS.md`) are editable
following normal review.

User-facing behavior changes need both a CHANGELOG entry AND, where
relevant, an update to the matching doc under `docs/`.
