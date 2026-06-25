# skm

> Declaratively manage agent skills.

`skm` installs the files of a _declared exact version_ of a skill into the
_correct location_ for an agent (Claude Code or Codex), and guarantees
**reproducibility**: the same `skm.lock`, on any machine, via
`skm sync --frozen`, produces skill directories with byte-for-byte identical
content hashes.

It does **two things and only two things**:

1. Place the declared content at the right place.
2. Make that placement reproducible.

It deliberately does **not** parse `SKILL.md` semantics, perform security
scanning, or manage plugins / hooks / settings.

## Features

- **Dual manifest model** (pnpm-style): a per-project `skm.toml` and a global
  `~/.config/skm/skm.toml`, selected with `-g`/`--global`.
- **Four source types**: `git`, `tar`, `zip`, and `local`.
- **Content lock, not dependency lock**: `skm.lock` records the SHA-256 of the
  installed file tree; `--frozen`/`--locked` gate only on that.
- **Multi-agent deployment**: one skill can deploy to both `claude`, `codex` and
  more; each lands atomically and independently.
- **Safe by default**: skm only touches skills it installed itself. Foreign
  (manually placed) directories are never auto-deleted.
- **Content-addressed cache** under `~/.cache/skm/` for fast, offline-capable
  re-installs.

> **Platform support:** macOS and Linux (including WSL). Native Windows is
> blocked at compile time; run skm inside WSL.

## Install

Requires a Rust toolchain (1.85+, edition 2024) and the system `git`.

```sh
# From a checkout:
cargo install --path .

# Or build a release binary:
cargo build --release   # → target/release/skm
```

## Quick start

```sh
# 1. Create a manifest in your project, then edit skm.toml:
#    uncomment `agents = […]` under [defaults] and pick your agent(s).
#    (Or skip this and pass --agent on each `skm add`.)
skm init

# 2. Add skills. Specs are auto-detected: owner/repo → GitHub, ./path →
#    local, URL suffix → tar/zip, otherwise git. Each command edits the
#    manifest, resolves, locks, and deploys in one step.
skm add anthropics/skills --subdir docx     # GitHub owner/repo + subdirectory
skm add anthropics/skills@v1.2              # pin with an inline @ref
skm add ./vendor/reviewer                   # local directory
skm add foo.tar.gz --sha256 <hex>           # archive, byte-verified

# 3. Remove a skill from the manifest (the on-disk copy is pruned on the
#    next sync; pass --no-sync to defer both the lock update and deploy).
skm rm docx

# 4. See what is installed.
skm status

# 5. Reproduce exactly elsewhere (CI):
skm sync --frozen
```

## Manifest (`skm.toml`)

Flat, Cargo-style. Exactly one of `git` / `tar` / `zip` / `local` per skill.

```toml
version = 1                    # required

[defaults]
agents = ["agents"]            # default agents; each skill may override
                               # (left unset by `skm init` — choose one or more, or pass --agent on each add)

[[skills]]
name   = "docx"                # install directory name (unique)
git    = "https://github.com/anthropics/skills"
ref    = "main"                # optional: branch / tag / 40-hex commit
subdir = "docx"                # optional: subdirectory inside the source

[[skills]]
name   = "my-tool"
zip    = "https://example.com/skills/my-tool.zip"
sha256 = "a1b2c3…"             # optional: verifies the downloaded bytes
subdir = "my-tool"
agents = ["agents", "claude"]

[[skills]]
name  = "team-reviewer"
local = "./vendor/reviewer"    # relative to this skm.toml
```

Skills install under:

| scope             | agents                      | claude                      | codex                      |
| ----------------- | --------------------------- | --------------------------- | -------------------------- |
| global (`-g`)     | `~/.agents/skills/`         | `~/.claude/skills/`         | `~/.codex/skills/`         |
| project (default) | `<project>/.agents/skills/` | `<project>/.claude/skills/` | `<project>/.codex/skills/` |

## Commands

| command                                                                                 | description                                                     |
| --------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `skm init [--force]`                                                                    | Write a commented `skm.toml` template.                          |
| `skm lock [--upgrade [<name>…]]`                                                        | Resolve the manifest → write `skm.lock` (no deploy).            |
| `skm sync [--frozen] [--locked] [--dry-run] [--offline] [--prune] [--no-prune] [--yes]` | The core convergence command: lock new/changed, deploy, prune.  |
| `skm add <spec> [opts]` (`a`)                                                           | Edit manifest → lock → sync. `--no-sync` defers deploy.         |
| `skm update [<name>…] [--dry-run] [--offline] [--no-prune] [--yes]` (`up`, `upgrade`)   | Re-resolve mutable refs and deploy (`lock --upgrade` + `sync`). |
| `skm remove <name>…` (`rm`)                                                             | Remove from the manifest; prune on the next sync.               |
| `skm status` (`st`)                                                                     | Show the state of every `(skill, agent)`.                       |
| `skm doctor`                                                                            | Diagnose the environment, cache, and exec-bit integrity.        |
| `skm cache <dir\|clean>`                                                                | Inspect or clear the global source cache.                       |

### Updating

`skm update` re-resolves mutable refs (a git branch/tag, or an archive URL
without a pinned `sha256`) and deploys the result in one step — the convenience
equivalent of `skm lock --upgrade` followed by `skm sync`. Bare `skm update`
upgrades every skill; pass names to narrow it (`skm update docx my-tool`).
Immutable pins (a 40-hex git commit, or a declared archive `sha256`) are no-ops,
so an update never moves a version you deliberately froze.

### Sync modes

- **default** — lock new/changed items, deploy, and prune managed extras (with
  confirmation).
- **`--frozen`** — content is fully determined by the lock; do not re-resolve.
  `agents` is still read from the manifest. A cache miss still hits the network
  unless `--offline` is given.
- **`--locked`** — assert `lock ≡ manifest`, then deploy. Does not write the
  lock. The CI gate.
- **`--dry-run`** — print the change plan; never write the lock or skills.
- **`--offline`** — never touch the network; use only the local cache.

### Exit codes

| code | meaning                                   |
| ---- | ----------------------------------------- |
| 0    | success, no change                        |
| 1    | general error                             |
| 2    | drift reported by `status` / `doctor`     |
| 3    | lock mismatch under `--locked`/`--frozen` |
| 4    | network error                             |
| 5    | permission / IO error                     |
| 6    | lock missing under `--frozen`/`--locked`  |
| 10   | `--dry-run` detected pending changes      |

## Reproducibility & ownership in one paragraph

A skill directory on disk is **owned** by skm if and only if its `name` appears
in the `skm.lock` that owns that root. Owned-but-undeclared directories are
**managed extras** and are pruned by default. Directories skm never installed
are **foreign** and are never auto-deleted. `local` sources are mutable path
dependencies (re-read every sync); for absolute reproducibility, use `git` or
`tar`.

## Operations & troubleshooting

### Authentication

- **git** uses your system git credential helper / SSH agent. All remote git
  calls run with `GIT_TERMINAL_PROMPT=0`, so a missing credential **fails fast**
  (exit 4) instead of hanging on a prompt — important in CI. For private repos,
  configure a _non-interactive_ helper first: `gh auth setup-git`, the system
  keychain, or `GIT_ASKPASS`.
- **zip / tar downloads do not support HTTP authentication.** For an
  authenticated private archive, use a `git` source, or pre-download it and
  point a `local` source at it.

### CI

```sh
skm sync --locked            # assert lock ≡ manifest, then deploy
skm sync --frozen            # content fully from the lock; ignore manifest source edits
skm sync --frozen --offline  # air-gapped: never touch the network
```

- `--locked` is the gate: exit 3 if the manifest and lock disagree, exit 6 if
  the lock is missing — so commit your `skm.lock`.
- Pruning needs a TTY to confirm; in CI pass `--yes` (or `--no-prune`),
  otherwise a prune-needed sync errors.

### Recovery

- **Lost `skm.lock`** — run a plain `skm sync`: it re-resolves, rewrites the
  lock, and re-claims on-disk directories whose content matches. `--locked` /
  `--frozen` instead fail loudly (they cannot assert ownership without a lock).
  If a directory on disk differs from what would be installed, sync aborts
  rather than overwrite it — resolve by hand (`rm -r <dir>`) and re-run.
- **Leftover `*.skm-new.*` / `*.skm-old.*`** (crash mid-deploy) — the next
  `skm sync` rolls back or cleans them automatically, and `skm doctor` reports
  them. In the rare case skm cannot auto-roll-back, it prints the exact
  `rm -rf <final> && mv <old> <final>` command to restore the previous version.
- **Reclaim disk space** — `skm cache clean` clears the global source cache. Run
  it when no sync is in progress (it refuses while one is actively downloading).

## Development

```sh
cargo build                 # debug build
cargo test                  # unit + integration tests (no external network)
cargo fmt --all             # format
cargo clippy --all-targets -- -D warnings   # lint (matches CI)
```

Integration tests build local git repositories and serve zip/tar archives from a
throwaway in-process HTTP server, so the whole suite runs offline.

Environment overrides (handy for CI / sandboxes):

- `SKM_CONFIG_DIR` — overrides `~/.config/skm/`.
- `SKM_CACHE_DIR` — overrides `~/.cache/skm/`.

See [`CLAUDE.md`](CLAUDE.md) for architecture, invariants, and design rationale.
Behavior is specified by the black-box suite in `tests/`.

## License

[MIT](LICENSE)
