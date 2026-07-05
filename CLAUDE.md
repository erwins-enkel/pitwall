# pitwall

## Idea

See /tmp/handoff-runner-stats-tui.md

## Notes

<!-- Project conventions go here. The first agent session authors the PRD. -->

## AI agent house rules

A Rust project. The goal: an agent ships here with minimal human back-and-forth
because deterministic guardrails do the coaching — run the gate, read the failure,
self-correct. A human should never be pulled in to re-explain a mechanical defect.

### First: activate the guardrails

`core.hooksPath` is local, unversioned git config, and the hooks call two extra
tools (`cog`, `cargo-machete`) that a fresh clone lacks. Wire up both in one step:

```sh
make setup        # = make tools + make hooks
```

`make tools` runs the two `cargo install`s below; `make hooks` points git at
.githooks/ (`git config core.hooksPath .githooks`). Run them by hand if you prefer:

```sh
cargo install cargo-machete
cargo install cocogitto --version 7.0.0   # `cog` (Conventional Commits linter)
```

### Engineering posture (surgical & mechanical)

- **Simplicity first.** Write the minimum code that solves the stated problem — no
  speculative features, abstractions for single use, or config nobody asked for.
- **Surgical changes.** Every changed line traces to the request. Don't refactor,
  reformat, or polish adjacent code; match existing style. Delete only what your
  change orphaned; surface pre-existing dead code rather than silently expanding the diff.
- **Fail closed.** Render error/unreachable paths as explicit failures; never let a
  swallowed error, empty result, or zero count masquerade as success.
- **Single source of truth.** Derive counts, totals, and dimensions from the data
  (array length, column count) — never hardcode a magic number that silently drifts.
- **Keep names and comments honest.** When you change a function's behavior, update
  its name, doc comment, and inline comments to match — no stale comment left behind.
- **No dead code.** Delete unreachable branches and unused fields/params, or wire
  them to a real path, before opening a PR.
- **Verify before claiming done.** Run the gate; show the output. Evidence before assertions.

### Guardrails (what enforces the above)

| Guardrail | Runs | Command |
| --- | --- | --- |
| Format | `pre-commit` hook + CI | `cargo fmt --check` |
| Lint (deny warnings) | `pre-push` hook + CI | `cargo clippy --all-targets --locked -- -D warnings` |
| Tests | `pre-push` hook + CI | `cargo test --locked` |
| Unused deps | `pre-push` hook + CI (`machete` job) | `cargo machete` |
| Commit-message format | `commit-msg` hook + CI (`commits` job) | `cog verify` / `cog check` |
| Dependency updates | Dependabot (`cargo` + `github-actions`, weekly) | — |

The `pre-push` hook mirrors CI, so a red CI is caught before it costs a round-trip.
`pre-commit` is intentionally kept to `cargo fmt --check` only so commits stay fast;
the full gate runs at push time. Local hooks fail **open** when `cargo-machete`/`cog`
isn't installed (they print an install hint) — CI is the authoritative gate.

### Commits

Conventional Commits, enforced by cocogitto and consumed by release-please for
versioning/changelog (see `.github/workflows/release.yml`). Use `feat:`, `fix:`,
`chore:`, `ci:`, `build:`, `docs:`, etc. Dependabot is configured to use
`build`/`chore`/`ci` prefixes so its PRs pass the `commits` gate.
