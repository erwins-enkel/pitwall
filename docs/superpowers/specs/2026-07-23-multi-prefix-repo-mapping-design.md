# Multi-prefix runner matching with per-prefix repo mapping

## Goal

Match self-hosted runner containers under **several** name prefixes, and let each
prefix resolve its jobs against its **own** repo — so a second runner fleet
(`flowagent-ci-runner-*`) shows real job/branch detail instead of always-idle
rows.

## Motivation (verified against live rootless docker)

Two fleets run side by side under one daemon:

| Container | `REPO_URL` | `RUNNER_NAME` |
| --- | --- | --- |
| `pulse-ci-runner-1` | `github.com/erwins-enkel/pulse` | `runner-1` |
| `flowagent-ci-runner-1` | `github.com/ltdovr/flowagent` | `runner-1` |

Today `Config.prefix` is a single `String` matched by `starts_with`, so only one
fleet is ever visible.

## Alternatives considered

**Cheaper option — multi-prefix match, no mapping.** Match several prefixes and
tag every unmapped fleet's runner with `key: None`. `model::join` short-circuits
a `None` key (`r.key.as_ref().and_then(|k| jobs.get(k))`), so those rows show
CPU/mem and always render idle — safe, no job mis-attribution, far less code. This
fully satisfies the literal "runners don't show" ask.

**Why the fuller feature.** The user was shown this exact tradeoff and explicitly
chose per-prefix repo mapping so the second fleet shows real job/branch detail
rather than idle rows. The justification is that job-detail goal.

Note on collision: a prior draft claimed the shared `runner-1` name *forces*
mapping. It does not. A `key: None` runner never matches a job, so stats-only does
not collide. The `RunnerKey` is `{ scope: repo, name }`; a collision would arise
only if both fleets were forced onto one shared **non-None** scope — which this
design never does (each fleet gets its own mapped scope, and an unmapped fleet
falls back to the user's first configured repo, not another fleet's).

## Configuration

`Config.prefix: String` becomes `Config.prefixes: Vec<PrefixRule>` where:

```rust
pub struct PrefixRule {
    pub r#match: String,      // container-name prefix
    pub repo: Option<String>, // owner/repo this fleet's jobs live under
}
```

### TOML `prefix`

Two accepted forms (trimmed to the minimum): a plain string, or an array of
tables. Array-of-strings and mixed string/table arrays are **not** supported —
multiple *unmapped* prefixes use the table form with `repo` omitted.

```toml
prefix = "pulse-ci-runner-"                                # string (backward compatible)
prefix = [                                                 # array of tables
  { match = "pulse-ci-runner-",     repo = "erwins-enkel/pulse" },
  { match = "flowagent-ci-runner-", repo = "ltdovr/flowagent" },
]
prefix = [{ match = "pulse-ci-runner-" }, { match = "flowagent-ci-runner-" }]  # two unmapped
```

Implemented as a new untagged enum
`PrefixField { One(String), Many(Vec<PrefixEntry>) }`, where `PrefixEntry` is a
`#[serde(deny_unknown_fields)]` struct `{ #[serde(rename = "match")] match_: String,
repo: Option<String> }`. `match` is required, so a table entry missing it is a hard
parse error.

### Env `PITWALL_PREFIX`

Comma-separated entries, each with an optional `=owner/repo` suffix:

```sh
PITWALL_PREFIX="pulse-ci-runner-=erwins-enkel/pulse,flowagent-ci-runner-=ltdovr/flowagent"
PITWALL_PREFIX="pulse-ci-runner-,flowagent-ci-runner-=ltdovr/flowagent"  # mixed
PITWALL_PREFIX="pulse-ci-runner-"                                        # today's form
```

Env overrides the whole `prefix` key (unchanged precedence). A bare entry maps to
`repo: None`.

### Default

Empty list resolves to `[{ match: "ci-runner-", repo: None }]` — identical to
today's single-prefix default.

## Scope tagging and the shared-scope collision

Each runner's key is built directly from its matching rule:
`rule.repo.map(|scope| RunnerKey { scope, name })`. A rule **with** a repo yields
a scoped key (job detail); a rule **without** a repo yields `key: None`, which
`join` short-circuits to an always-idle row (CPU/mem shown, no job lookup). The
collector has **no** `configured_repos.first()` fallback.

**Why no fallback.** `docker_runner_name` maps both `pulse-ci-runner-1` and
`flowagent-ci-runner-1` to `runner-1`. If the collector tagged every unmapped
fleet with a shared non-None scope (e.g. `configured_repos.first()`), two unmapped
fleets would produce the *same* `RunnerKey { erwins-enkel/pulse, runner-1 }` and
`join` would attribute one job to both rows. `key: None` for unmapped fleets makes
that impossible.

### Backward-compat implicit mapping (first rule only)

Today `prefix = "ci-runner-"` with a single `repo` tags docker runners with the
first configured repo, and job detail works. To preserve that, `config::resolve`
folds the implicit mapping into the rule list: if `prefixes[0].repo` is `None` and
`configured_repos` is non-empty, `prefixes[0].repo` is set to
`configured_repos.first()`. Every *other* unmapped rule stays `None` (idle). The
special case lives in one explicit, testable step; the collector stays uniform.

## Auto-poll of mapped repos

`configured_repos` stays the user's list. A pure helper
`prefix_poll_seed(configured: &[String], prefixes: &[PrefixRule]) -> Vec<String>`
returns `configured` followed by each rule's `repo` (when set), order-preserving
and deduped. `app.rs` feeds that seed to `resource_native::derive_scopes` (in
place of `&cfg.configured_repos`), so mapped repos are polled for job detail and
feed the `multi_repo` column gate. `derive_scopes` is unchanged. The folded
first-rule repo equals `configured_repos.first()`, so it is already in the seed;
dedup keeps it single.

## Matching (`resource_docker.rs`)

`container_matches(name, prefix) -> bool` becomes
`matching_rule<'a>(name: &str, rules: &'a [PrefixRule]) -> Option<&'a PrefixRule>`
— returns the first rule (config order) whose `match` is a prefix of the
leading-`/`-trimmed name.

`collect` takes `&[PrefixRule]` instead of the old `prefix: &str` + `repo: &str`
(no fallback scope). Per container:

- skip when `matching_rule` is `None`;
- otherwise the runner's key is `rule.repo.map(|scope| RunnerKey { scope, name })`
  — a mapped rule yields a scoped key (job detail); an unmapped rule yields
  `key: None` (idle). Backward compat for the first prefix comes from the implicit
  first-repo folding done in `config::resolve` (above), so `rule.repo` is already
  populated for the historical single-prefix case.

`run` passes `&cfg.prefixes`; no scope argument.

## UI (`ui.rs`, `app.rs`)

`View.prefix: &'a str` is unchanged in type. `app.rs` builds it by comma-joining
the rule `match` strings (e.g. `"pulse-ci-runner-, flowagent-ci-runner-"`). The
existing empty-state format string renders
`N containers running, none match prefix 'pulse-ci-runner-, flowagent-ci-runner-'`.
No change to `ui.rs` logic or the ~15 `View` test literals or
`examples/screenshot.rs`.

## Known limitation: shared memory gauge

`model::slice_total_bytes` sums **all** docker rows. A second matched fleet is
therefore added into the single memory gauge capped by `slice_cap_gib` (default
24 GiB): the gauge stops meaning "the pulse slice" and becomes "all matched docker
runners". Per-prefix caps are out of scope; the README notes this so the number is
read correctly.

## Docs

- README config table: document the string and table TOML forms, the env
  `=owner/repo` suffix, and the shared-gauge limitation above.
- `config.example.toml`: show the table form alongside the plain string form.

## Testing seams

- `config.rs`: pure `resolve()` tests for the string form, table form (mapped and
  unmapped), env plain and `=repo` forms, dedup, default, env-overrides-file, and
  the **implicit first-repo folding** (single unmapped prefix inherits
  `configured_repos.first()`; two unmapped prefixes → only `prefixes[0]` gets the
  repo, `prefixes[1].repo` stays `None`). `prefix_poll_seed` merge/dedup test
  (mapped repos appended, `configured_repos` untouched). New `load_file` test for
  the table TOML form and a negative test for a table entry missing `match`.
  Update `shipped_example_config_loads` for the new example content.
- `resource_docker.rs`: `matching_rule` unit tests — first-match-wins ordering,
  leading-slash trim, no-match. **Scope-selection (a) mapped vs mapped**: rules
  `[{ pulse-ci-runner-, Some(erwins-enkel/pulse) }, { flowagent-ci-runner-,
  Some(ltdovr/flowagent) }]` → distinct scoped keys per fleet.
  **Scope-selection (b) two unmapped (collision case)**: rules
  `[{ pulse-ci-runner-, Some(erwins-enkel/pulse) }, { flowagent-ci-runner-,
  None }]` (the post-`resolve` folding of two unmapped prefixes with
  `configured_repos = ["erwins-enkel/pulse"]`) → `flowagent-ci-runner-1` gets
  `key: None`, so it does **not** inherit pulse's `runner-1` job. Paired config
  test: `resolve` folds two unmapped prefixes to `[{pulse, Some(pulse)},
  {flowagent, None}]`.

## Out of scope

- Regex matching.
- Array-of-strings and mixed string/table TOML `prefix` forms.
- Per-prefix slice caps or any other per-prefix config.
- Runner-name derivation changes (both fleets use `runner-N`; existing
  derivation already resolves them).
- Scoping a single container to one of several unmapped repos.

## Assumptions

- Runner containers register their GitHub name as `runner-N` (verified for both
  live fleets).
- `PITWALL_PREFIX` continuing to override the entire `prefix` key (not merge with
  the file) is acceptable — consistent with every other env var.
```
