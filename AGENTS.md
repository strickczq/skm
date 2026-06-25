# AGENTS.md

Guidance for AI/code agents working in this repository.

## What this is

`skm` is a Rust CLI that declaratively manages agent skills. This file is the
agent's **operations manual + index** — commands, conventions, the invariants
you must not break, and pointers to where things live. It is **not** a spec:
behavior is owned by the black-box suite in `tests/` (the executable spec), and
design rationale — the "why" — lives in a **comment next to the code it
explains**, with the few genuinely cross-cutting choices in
[`docs/decisions.md`](docs/decisions.md).

## Key commands

```sh
cargo build                                   # build
cargo test                                    # all tests (offline, self-contained)
cargo test --test source_git                  # one integration suite
cargo fmt --all                               # format
cargo clippy --all-targets -- -D warnings     # lint exactly as CI does
cargo run -- <args>                           # run the CLI locally
```

CI (`.github/workflows/ci.yml`) runs fmt-check, clippy `-D warnings`, build, and
test on Linux and macOS. Keep all four green.

## Architecture

Pipeline: **manifest → resolve (lock phase) → deploy (sync phase)**.

```
src/
  main.rs        CLI definition (clap) + dispatch; #[cfg(windows)] compile guard
  cmd/           one module per subcommand (init/lock/sync/add/update/remove/
                 status/doctor/cache) + shared flock/GC helpers in cmd.rs.
                 update = lock --upgrade + sync (delegates to sync::execute)
  pipeline/      the two-phase core:
    resolve.rs   lock phase: identity comparison, network resolve, hashing
    deploy.rs    deploy phase: planning, atomic landing, prune, ghost
  model/         typed models of persistent state:
    agent.rs     Agent enum + Scope → skills_root
    config.rs    scope + Workspace resolution; config/cache dirs (env-overridable)
    manifest.rs  skm.toml: serde read + toml_edit rewrite (add/remove)
    lockfile.rs  skm.lock: serde read + deterministic toml_edit write, atomic write
  source/        Source trait-ish enum + git/tar/zip/local backends
  sys/           domain-free mechanism (no skm semantics):
    hash.rs      deterministic content_sha256 (materialize + verify in one fn)
    fsutil.rs    flock, copy_tree (reflink), atomic rename, crash-recovery GC
    cache.rs     content-addressed source-artifact cache
  error.rs       SkmError + the documented exit codes
  ui.rs          terminal output: color/prefix, stdout vs stderr routing
```

Layering (single direction, no cycles): `cmd` → `pipeline` → `model` / `source`
→ `sys`; `error` and `ui` are cross-cutting leaves used everywhere.
`skm doctor`'s diagnostics live in `cmd/doctor.rs`.

### Invariants to preserve (do not break these)

- **I1** `(scope, agent)` → exactly one skills root.
- **I2** ownership = lock: a dir is owned iff its name is in `skm.lock`.
- **I3** the lock gates on `content_sha256` only (not source hashes).
- **I4** resolve (what to install) and deploy (where) are decoupled; `agents`
  are never written to the lock.
- **H1** the hash of a freshly materialized tree must equal the hash recomputed
  from the installed on-disk dir, per source policy. `hash::hash_tree` is the
  single implementation; the git/tar exec bit is set on the staging tree at
  materialize time so both contexts read `st_mode` and agree.

(The _why_ lives next to the code; the cross-cutting lock/concurrency rationale
is in [`docs/decisions.md`](docs/decisions.md).)

### Things easy to get wrong

- **Exec bit ladder**: git/tar include it in the hash and `chmod +x` on landing;
  zip lands 0644 and is excluded; local is copied verbatim and excluded.
- **Foreign guard**: uses the _pre-sync_ lock (ownership snapshot) to decide
  foreign vs managed, not the freshly-written lock.
- **Prune/ghost**: scan **all** agent-enum roots, not the union of manifest
  agents. Ghost cleanup uses the disk snapshot taken **before** prune (two-pass
  convergence is intended, not a bug).
- **`--frozen` still materializes every entry** to verify content against the
  lock (that is how local-source drift is caught) and still reads `agents` from
  the manifest.
- **Deterministic output**: lockfile skills are sorted by name in UTF-8 byte
  order; lock writes are skipped when nothing substantively changed
  (`generated_by` is ignored in the comparison).

## Development workflow for agents

1. Read the test(s) in `tests/` that pin the behavior (and `docs/decisions.md`
   for the rationale) before changing behavior.
2. Make the change with a matching unit or integration test (tests live in
   `tests/`; `tests/support/` provides an isolated `Env`, local git repos, and
   in-process HTTP mocking via `mockito` — no external network is ever
   required). For new behavior, write the failing test first.
3. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and
   `cargo test` before committing.
4. Keep commits small and logical; name the externally-visible contract change
   (or note it's a pure refactor) in the message.
5. Never weaken an invariant (I1–I4, H1) or the "never destroy foreign assets"
   rule without updating both the test that pins it and the rationale (a code
   comment, or `docs/decisions.md` for the cross-cutting ones).

## Test isolation note

Tests set `HOME`, `SKM_CONFIG_DIR`, and `SKM_CACHE_DIR` to temp dirs so they
never touch a real `~/.config` or `~/.cache`. When adding tests that resolve the
global scope, set those env vars the same way.
