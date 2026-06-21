# Agent guidelines

This file is the entry point for AI agents working in this repository. Read
it before touching anything. It captures project-specific conventions that
are not derivable from the code alone.

Before writing, modifying, or archiving documentation, read and follow
`docs/dev/documentation/index.md`.

## 1. Pre-flight

Run these at the start of every editing session:

```bash
# Recent commit message style; match it.
git --no-pager log --oneline -15

# Tag type the project uses.
git cat-file -t "$(git describe --abbrev=0)"

# Current host version source.
grep -n '^version =' crates/typio-host/Cargo.toml | head -1

# Pending CHANGELOG entries.
sed -n '/^## \[Unreleased\]/,/^## \[/p' CHANGELOG.md
```

If any of these surprise you, stop and reconcile them with the planned
change. Project convention beats generic defaults.

## 2. Commit Message Conventions

The repo uses Conventional-Commits-style prefixes observed in history:

| Prefix | Use |
|---|---|
| `feat:` | New user-visible feature |
| `fix:` or `fix(scope):` | Bug fix |
| `test:` or `test(scope):` | Test-only change |
| `docs:` | Documentation only |
| `build:` | Build system or dependency wiring |
| `ci:` | CI config |
| `release: vX.Y.Z` | Release commit |
| `subsystem:` | Looser historical form; prefer conventional prefixes |

Rules:

- Subject <= about 70 characters, lowercase, no trailing period.
- Body wraps at about 72 columns and explains why.
- Reference ADRs by ID when applicable, such as `(ADR-0035)`.
- Never add `Co-Authored-By:` or agent attribution.
- Always commit with `-m "subject"` and optional `-m "body"`; never launch
  an editor from an agent shell.

## 3. Tag Conventions

All release tags in this repo are annotated:

```bash
git cat-file -t v0.3.3
# tag
```

Create release tags with an inline message:

```bash
git tag -a vX.Y.Z -m "release: vX.Y.Z"
```

Do not use `git tag vX.Y.Z`, `git tag -a vX.Y.Z` without `-m`, or
`git tag -a vX.Y.Z -m ""`.

If an editor opens during a git operation, git is asking for a message.
Supply `-m` to the command that wanted the message.

## 4. Release Workflow

A release is two commits plus one tag, in this order:

1. Land substantive commits first. CHANGELOG entries go under
   `## [Unreleased]`.
2. Release commit: bump `version = "X.Y.Z"` in
   `crates/typio-host/Cargo.toml`, update `Cargo.lock`, and move the
   `## [Unreleased]` block to `## [X.Y.Z] - YYYY-MM-DD` in
   `CHANGELOG.md`. Use commit title `release: vX.Y.Z`.
3. Annotated tag `vX.Y.Z` with message `release: vX.Y.Z`.
4. Push `main` and the tag.

Version bump rules:

- Patch: bug fixes and internal improvements with no user-visible behavior
  change.
- Minor: user-visible new feature or behavior.
- Major: incompatible change.

Patch releases do not need a new empty `## [Unreleased]` placeholder above
the released block; the next feature commit recreates it.

## 5. Version Source

The host version source is `crates/typio-host/Cargo.toml`:

```toml
[package]
name = "typio-host"
version = "0.4.0-dev"
```

Do not add a second version source.

## 6. CHANGELOG Format

Use Keep a Changelog order:

`### Added` -> `### Changed` -> `### Deprecated` -> `### Removed` ->
`### Fixed` -> `### Security`.

Within a section, bullet entries start with a bold lead phrase:

```markdown
- **Cargo-only host build.** The host daemon now builds and installs through
  Cargo...
```

Date format is `YYYY-MM-DD`. Get it from `date -I` or `date +%F`.

## 7. Build, Test, and Verification Gates

Before committing non-trivial host changes:

```bash
( cd ../libtypio && cargo build --release )
( cd ../../flux && meson setup build && meson compile -C build )

export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build --release -p typio-host --bin typio
cargo test -p typio-host
```

`flux` is still a native C library with a Meson build tree; that is a
sibling-repo build prerequisite, not typio-linux's build system.

## 8. Repo Layout and Cross-Repo Work

Sibling projects live under `/home/ming/projects/typio/`:

| Path | Role |
|---|---|
| `typio-linux/` | This repo, Rust Wayland host daemon |
| `libtypio/` | Rust framework library |
| `flux/` | Vulkan GPU canvas library used by the Panel |
| `typio-engine-*` | Engine plugins |
| `typio-settings/`, `typioctl/`, `typio-vet/`, `typio-docs/` | Tools and docs |

Cross-repo edits are allowed when the fix genuinely belongs in a sibling
repo. When touching a sibling repo:

- Read its own `AGENTS.md` or `CLAUDE.md` first.
- Do not bump sibling versions in lockstep unless asked.
- Keep dependency pin changes deliberate and separate when they affect CI.

## 9. Common Agent Failure Modes

| Symptom | Wrong reaction | Right reaction |
|---|---|---|
| `$EDITOR` opens during git | Try pager flags | Supply `-m` to the git command |
| Cargo test loads old `libflux.so` | Patch around missing symbols | Rebuild `../../flux` and check `LD_LIBRARY_PATH` / RUNPATH |
| Unsure whether a version bump is patch or minor | Default to minor | Default to patch unless behavior changes |
| Missing project fact | Guess | Run the pre-flight and inspect current files |

## 10. Documentation Governance

`docs/dev/documentation/` has routing and style rules. AI agents may read it
and suggest changes, but must not directly modify files in that directory.
Other docs, including `README.md`, `CONTRIBUTING.md`, `CHANGELOG.md`,
`AGENTS.md`, and `docs/dev/*.md`, are editable following normal review.

User-facing behavior changes need both a CHANGELOG entry and, where
relevant, an update to the matching doc under `docs/`.
