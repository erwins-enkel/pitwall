# Multi-repo Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let pitwall poll multiple repos — `PITWALL_REPO` accepts comma-separated values and the TOML `repo` key accepts a string or an array — for both self-hosted runner job detail and the hosted-jobs section.

**Architecture:** The poller already iterates `Config.repos: Vec<String>` per scope concurrently. This change widens only the config surface: resolve a `Vec` of configured repos, seed `derive_scopes` from that list, and remove the now-unnecessary `DEFAULT_REPO` sentinel (empty `Vec` = unset). The type/signature changes span config.rs, resource_native.rs, jobs.rs, and app.rs, so they land in one compiling commit.

**Tech Stack:** Rust, serde (untagged enum for the string-or-array TOML field), tokio.

## Global Constraints

- Edition 2021, no new dependencies.
- Backward compatible: `PITWALL_REPO=x/y` and `repo = "x/y"` still resolve to a one-element list and behave exactly as before.
- Env overrides file (unchanged precedence). Values trimmed; empty entries dropped; final list order-preserving deduped.
- `cargo fmt`/`cargo clippy --all-targets -D warnings` clean (CI gate). Pre-commit hook runs fmt+clippy+test.

---

### Task 1: Multi-repo config resolution + scope seed

**Files:**
- Modify: `src/config.rs` (RepoField, helpers, `resolve`, `Config` field, remove `DEFAULT_REPO`, tests)
- Modify: `src/resource_native.rs` (`derive_scopes` signature + seed, test)
- Modify: `src/jobs.rs` (drop `DEFAULT_REPO` filter, empty-check hint)
- Modify: `src/app.rs` (pass `&cfg.configured_repos`)

**Interfaces:**
- Produces: `Config.configured_repos: Vec<String>` (replaces `Config.repo: String`); `derive_scopes(configured: &[String], runners: &[NativeRunner]) -> (Vec<String>, Vec<String>)`.
- `DEFAULT_REPO` const is removed.

- [ ] **Step 1: Write the failing/updated tests**

In `src/config.rs` tests module, ADD:

```rust
// ---- split_repos / dedup_preserve -----------------------------------

#[test]
fn split_repos_trims_and_drops_empties() {
    assert_eq!(split_repos("a/b, c/d"), vec!["a/b", "c/d"]);
    assert_eq!(split_repos("  x/y  "), vec!["x/y"]);
    assert_eq!(split_repos("a/b,,c/d,"), vec!["a/b", "c/d"]);
    assert!(split_repos("").is_empty());
    assert!(split_repos("  ,  ").is_empty());
}

#[test]
fn dedup_preserve_keeps_first_occurrence_order() {
    assert_eq!(
        dedup_preserve(vec![
            "a/b".into(),
            "c/d".into(),
            "a/b".into(),
            "e/f".into()
        ]),
        vec!["a/b", "c/d", "e/f"]
    );
}

// ---- resolve() multi-repo -------------------------------------------

#[test]
fn resolve_comma_env_yields_multiple_repos() {
    let c = resolve(FileConfig::default(), &env(&[("PITWALL_REPO", "a/b, c/d")]));
    assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
}

#[test]
fn resolve_toml_array_yields_multiple_repos() {
    let file = FileConfig {
        repo: Some(RepoField::Many(vec!["a/b".into(), "c/d".into()])),
        ..Default::default()
    };
    let c = resolve(file, &env(&[]));
    assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
}

#[test]
fn resolve_toml_string_with_comma_splits() {
    let file = FileConfig {
        repo: Some(RepoField::One("a/b,c/d".into())),
        ..Default::default()
    };
    let c = resolve(file, &env(&[]));
    assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
}

#[test]
fn resolve_dedups_repeated_repos() {
    let c = resolve(
        FileConfig::default(),
        &env(&[("PITWALL_REPO", "a/b,a/b,c/d")]),
    );
    assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
}
```

UPDATE these existing `src/config.rs` tests (assertions on the removed `c.repo`):
- `defaults_when_env_and_file_empty`: replace `assert_eq!(c.repo, "owner/repo");` with `assert!(c.configured_repos.is_empty());`
- `file_values_used_when_env_unset`: change the builder line `repo: Some("o/r".into()),` to `repo: Some(RepoField::One("o/r".into())),`, and `assert_eq!(c.repo, "o/r");` to `assert_eq!(c.configured_repos, vec!["o/r"]);`
- `env_overrides_file`: change `repo: Some("file/repo".into()),` to `repo: Some(RepoField::One("file/repo".into())),`, and `assert_eq!(c.repo, "env/repo");` to `assert_eq!(c.configured_repos, vec!["env/repo"]);`
- `empty_env_treated_as_unset`: change `repo: Some("file/repo".into()),` to `repo: Some(RepoField::One("file/repo".into())),`, and `assert_eq!(c.repo, "file/repo");` to `assert_eq!(c.configured_repos, vec!["file/repo"]);`

UPDATE the FileConfig deserialize tests:
- `load_valid_toml_parses`: replace `assert_eq!(cfg.repo.as_deref(), Some("o/r"));` with `assert!(matches!(cfg.repo, Some(RepoField::One(ref s)) if s == "o/r"));`
- `shipped_example_config_loads`: replace `assert_eq!(cfg.repo.as_deref(), Some("your-org/your-repo"));` with `assert!(matches!(cfg.repo, Some(RepoField::One(ref s)) if s == "your-org/your-repo"));`
- ADD:

```rust
#[test]
fn load_toml_repo_array_parses() {
    let p = temp_path("repo-array.toml");
    std::fs::write(&p, "repo = [\"o/r\", \"a/b\"]\n").unwrap();
    let cfg = load_file(ConfigPath::Explicit(p.clone())).unwrap().unwrap();
    std::fs::remove_file(&p).unwrap();
    assert!(matches!(cfg.repo, Some(RepoField::Many(ref v)) if v == &["o/r", "a/b"]));
}
```

In `src/resource_native.rs` tests, UPDATE `derive_scopes_dedups_repos_and_collects_orgs` (~line 579): change the call `derive_scopes("erwins-enkel/pulse", &runners)` to `derive_scopes(&["erwins-enkel/pulse".to_string()], &runners)` (assertions unchanged). ADD:

```rust
#[test]
fn derive_scopes_seeds_from_multiple_configured_repos() {
    let (repos, orgs) = derive_scopes(
        &["a/b".to_string(), "c/d".to_string()],
        &[],
    );
    assert_eq!(repos, vec!["a/b", "c/d"]);
    assert!(orgs.is_empty());
}
```

- [ ] **Step 2: Run the new tests to verify they fail (won't compile yet)**

Run: `cargo test --lib config:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type RepoField`, `cannot find function split_repos`/`dedup_preserve`, `no field configured_repos`. (The crate won't compile until Step 3 is complete — this is the expected red state for a compile-coupled change.)

- [ ] **Step 3: Implement the config changes (`src/config.rs`)**

1. Remove the sentinel const:

```rust
// DELETE:
// pub const DEFAULT_REPO: &str = "owner/repo";
```

(and its doc-comment lines 8-10)

2. Add the `RepoField` enum near `FileConfig`:

```rust
/// The TOML `repo` key accepts either a single repo string or an array of them
/// (each entry may itself be comma-separated). Untagged so both forms parse
/// transparently.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepoField {
    One(String),
    Many(Vec<String>),
}
```

3. Change `FileConfig.repo`:

```rust
    repo: Option<RepoField>,
```

4. Add helpers (near `resolve`):

```rust
/// Split a repo spec on commas, trimming each entry and dropping empties.
fn split_repos(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

/// Order-preserving dedup — keeps the first occurrence of each repo.
fn dedup_preserve(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for s in v {
        if !out.iter().any(|e| e == &s) {
            out.push(s);
        }
    }
    out
}

/// Flatten a TOML `repo` field (string or array; entries may be comma-separated)
/// into a repo list.
fn repo_field_to_vec(f: Option<RepoField>) -> Vec<String> {
    match f {
        Some(RepoField::One(s)) => split_repos(&s),
        Some(RepoField::Many(v)) => v.iter().flat_map(|s| split_repos(s)).collect(),
        None => Vec::new(),
    }
}
```

5. Replace the `repo` resolution in `resolve()` (lines 155-157):

```rust
    // Env (comma-separated) wins over the file's string-or-array; empty ⇒ unset.
    let configured_repos = match env_nonempty(get, "PITWALL_REPO") {
        Some(env_val) => dedup_preserve(split_repos(&env_val)),
        None => dedup_preserve(repo_field_to_vec(file.repo)),
    };
```

6. Change the `Config` struct field (replace `pub repo: String`):

```rust
    /// User-configured repos (from `PITWALL_REPO` / the TOML `repo` key). Empty
    /// when unset. `app` seeds `derive_scopes` from this; the derived poll list
    /// lands in `repos`.
    pub configured_repos: Vec<String>,
```

7. Update the `Config { .. }` construction at the end of `resolve()` (replace the `repos: vec![repo.clone()]` and `repo,` lines):

```rust
    Config {
        repos: configured_repos.clone(),
        orgs: vec![],
        socket_path,
        configured_repos,
        prefix,
        slice_cap_bytes: cap_gib.saturating_mul(GIB),
        flavor,
        warn_ratio,
        crit_ratio,
    }
```

- [ ] **Step 4: Implement `derive_scopes` (`src/resource_native.rs`)**

Replace the signature and seed (lines 40-41):

```rust
pub fn derive_scopes(
    configured: &[String],
    runners: &[NativeRunner],
) -> (Vec<String>, Vec<String>) {
    let mut repos: Vec<String> = Vec::new();
    let mut orgs: Vec<String> = Vec::new();
    let push_unique = |v: &mut Vec<String>, s: &str| {
        if !v.iter().any(|e| e == s) {
            v.push(s.to_string());
        }
    };
    for c in configured {
        push_unique(&mut repos, c);
    }
```

(the rest of the function — the `for r in runners` loop and `(repos, orgs)` — is unchanged). Update the doc-comment's `[pulse_repo]` reference to `configured repos`.

- [ ] **Step 5: Implement the poller + app changes**

`src/jobs.rs`:
- Change the import (line 1): `use crate::config::Config;` (drop `, DEFAULT_REPO`).
- Replace the `real_repos` block and hint guard (lines 298-326) with:

```rust
        if cfg.repos.is_empty() && cfg.orgs.is_empty() {
            // No configured repos and no native scopes → nothing pollable.
            let _ = tx
                .send(JobsUpdate {
                    jobs: Slice::new(),
                    hosted: Vec::new(),
                    error: Some(
                        "PITWALL_REPO unset — set it to your runners' repo (e.g. myorg/myrepo)"
                            .into(),
                    ),
                })
                .await;
            tokio::time::sleep(Duration::from_secs(15)).await;
            continue;
        }

        let repo_futs = cfg
            .repos
            .clone()
            .into_iter()
            .map(|scope| poll_scope(scope, ScopeKind::Repo));
```

(the `org_futs` and everything after are unchanged).

`src/app.rs` (line 60): change `derive_scopes(&cfg.repo, &natives)` to `derive_scopes(&cfg.configured_repos, &natives)`.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test`
Expected: PASS across the crate (new + updated config/derive_scopes tests, all existing tests).

- [ ] **Step 7: Commit**

```bash
git add src/config.rs src/resource_native.rs src/jobs.rs src/app.rs
git commit -m "feat: accept multiple repos in PITWALL_REPO / TOML repo"
```

---

### Task 2: Docs — README + example config

**Files:**
- Modify: `README.md` (Configuration table `repo` row + example snippet)
- Modify: `config.example.toml` (show the array form as a comment)

- [ ] **Step 1: Update `config.example.toml`**

Under the existing `repo = "your-org/your-repo"` line, add a comment showing the multi-repo forms (keep the active key a single string so `shipped_example_config_loads` still asserts `RepoField::One("your-org/your-repo")`):

```toml
# repo may be one repo or several — a comma-separated string or an array:
#   repo = "your-org/your-repo, your-org/another-repo"
#   repo = ["your-org/your-repo", "your-org/another-repo"]
```

- [ ] **Step 2: Update `README.md` Configuration section**

In the settings table, change the `repo` row's description to note it accepts one or more repos (comma-separated in `PITWALL_REPO`; a string or array in the TOML `repo` key), and that every listed repo is polled for both runner job detail and the hosted-jobs section. In the example TOML config snippet, show the array form. Keep it concise and consistent with the existing table style.

- [ ] **Step 3: Verify the example still loads**

Run: `cargo test --lib config::tests::shipped_example_config_loads`
Expected: PASS (the active `repo` key remains a valid single string).

- [ ] **Step 4: Commit**

```bash
git add README.md config.example.toml
git commit -m "docs: document multi-repo repo config"
```

---

## Verification (after all tasks)

- [ ] `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` all clean.
- [ ] `PITWALL_REPO="erwins-enkel/pulse,scoop/kanban-api" cargo run` (or the installed binary) — confirm both repos' runners/jobs appear and the hosted section reflects both; a single `PITWALL_REPO=erwins-enkel/pulse` still behaves as before.
