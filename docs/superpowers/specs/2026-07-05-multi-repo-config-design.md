# Multi-repo configuration for pitwall

## Goal

Let a user configure **more than one repo** for pitwall to poll (job detail for
self-hosted runners AND the hosted-jobs section), instead of the current single
`PITWALL_REPO`. The internals already poll `Config.repos: Vec<String>` per scope
concurrently; this change only widens the config surface and the scope seed.

## Decision provenance

Follow-up to PR #25 (hosted-runner status). The user asked whether hosted jobs
show for one repo or the whole org; answer was repo-only. They then asked "but I
could give it multiple?" — today no (single repo). Chosen approach (explicit
answer during brainstorming):

- *"How should multiple repos be configured?"* → **Reuse `repo`, accept a list**
  — `PITWALL_REPO` accepts comma-separated values; the TOML `repo` key accepts a
  string OR an array. No new keys, fully backward compatible.

This is deliberately the bounded alternative to true org-wide coverage (which
would require enumerating org repos and has unbounded rate-limit cost) — the
user lists the exact repos they care about.

## Decisions

- **Config surface:** `PITWALL_REPO` is comma-separated (`a/b,c/d`); TOML `repo`
  accepts a string (optionally comma-separated) or an array of strings. Env
  overrides file (unchanged precedence). Values are trimmed; empty entries
  dropped; the final list is order-preserving deduped.
- **Backward compatibility:** `PITWALL_REPO=x/y` and `repo = "x/y"` behave
  exactly as before (a one-element list).
- **Unset = empty list.** With no configured repos and no native repo scopes,
  the poller shows the existing "PITWALL_REPO unset" hint. This replaces the
  `DEFAULT_REPO = "owner/repo"` sentinel, which is **removed** — an empty
  `Vec` is the natural "unset" signal, so the sentinel and its filter in
  `jobs.rs` are no longer needed.
- **No behavior change to polling, hosted collection, join, or native
  discovery** beyond seeding from a list.

## Config model (`src/config.rs`)

- `Config.repo: String` → `Config.configured_repos: Vec<String>` (the resolved
  user list; empty ⇒ unset). The derived `Config.repos: Vec<String>` (poll list,
  set by `app` from `derive_scopes`) and `Config.orgs` are unchanged.
- `FileConfig.repo: Option<String>` → `Option<RepoField>` where:

```rust
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepoField {
    One(String),
    Many(Vec<String>),
}
```

- New pure helpers:
  - `fn split_repos(s: &str) -> Vec<String>` — split on `,`, trim, drop empties.
  - `fn dedup_preserve(v: Vec<String>) -> Vec<String>` — order-preserving dedup
    (or fold into the resolve step).
  - Normalize `RepoField` → `Vec<String>`: `One(s)`/each `Many` element run
    through `split_repos`, then dedup.
- `resolve()` replaces the `repo` line with:

```rust
let configured_repos = match env_nonempty(get, "PITWALL_REPO") {
    Some(env_val) => dedup_preserve(split_repos(&env_val)),
    None => dedup_preserve(repo_field_to_vec(file.repo)),
};
```

- Remove `pub const DEFAULT_REPO`.

## Scope seed (`src/resource_native.rs`)

- `derive_scopes(pulse_repo: &str, runners)` →
  `derive_scopes(configured: &[String], runners) -> (Vec<String>, Vec<String>)`.
  Seed `repos` by `push_unique`-ing each configured repo (instead of
  `vec![pulse_repo]`), then append native repo scopes as today. Org collection
  unchanged. The existing order-preserving dedup handles overlap between
  configured repos and native repo scopes.

## Poller (`src/jobs.rs`)

- Drop the `DEFAULT_REPO` import and the `.filter(|r| r != DEFAULT_REPO)` step.
  Use `cfg.repos` directly; the unset hint fires on
  `cfg.repos.is_empty() && cfg.orgs.is_empty()`. Hint text unchanged.

## App wiring (`src/app.rs`)

- `derive_scopes(&cfg.repo, &natives)` → `derive_scopes(&cfg.configured_repos, &natives)`.

## Documentation (`README.md`)

- Update the Configuration table's `repo` row: note it accepts one or more
  repos — comma-separated in `PITWALL_REPO`, a string or array in the TOML
  `repo` key — and that all listed repos are polled for both runner job detail
  and the hosted-jobs section. Update the example config snippet to show the
  array form.

## Testing

- `split_repos`: `"a/b,c/d"` → two; whitespace trimmed; empties/trailing commas
  dropped; `""` → empty.
- `dedup_preserve`: duplicates collapse, order kept.
- `resolve` (via the existing pure-`resolve` test harness that injects env + a
  `FileConfig`):
  - unset → `configured_repos.is_empty()`.
  - single env `"o/r"` → `["o/r"]` (back-compat).
  - comma env `"a/b, c/d"` → `["a/b", "c/d"]`.
  - env overrides file when both set.
  - TOML `repo` as string and as array both resolve; comma inside a TOML string
    splits.
  - Update existing `resolve` tests that assert `c.repo == "owner/repo"` /
    `"o/r"` etc. to the new `configured_repos` Vec shape.
- `FileConfig` deserialize tests: `repo = "x/y"` and `repo = ["x/y","a/b"]` both
  parse into `RepoField`; update the two existing tests asserting
  `cfg.repo.as_deref()`.
- `derive_scopes`: seeds from a multi-element configured list, dedupes against an
  overlapping native repo scope, still collects orgs. Update the existing
  single-repo `derive_scopes` test to the slice signature.

## Out of scope

- Org-wide hosted/job coverage (enumerating all org repos) — separate feature.
- Per-repo prefixes/slice caps or any per-repo config beyond the repo list.
- Any change to hosted-job classification, rendering, or native discovery.
