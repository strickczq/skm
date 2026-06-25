# Design decisions (the "why")

Only decisions that span the whole pipeline and have **no single code home**
live here — everything else is a comment next to the code it explains, a test,
or deleted as duplication. Two entries. If this file starts collecting
one-liners again, it's drifting back into a catch-all; push those to the code
instead.

## The lock is content + ownership in one file

`skm.lock` plays two roles at once, and both are deliberate, cross-cutting
choices (they shape resolve, the lock schema, deploy, prune, and every CLI
command — no single file owns them):

- **It locks `content_sha256` of the installed tree, not the resolved commit.**
  A server can repack an archive — different compression, timestamps, file order
  — producing different raw bytes for identical content. So `archive_sha256` and
  `resolved_commit` are recorded but are **record-only**; only `content_sha256`
  gates `--frozen`/`--locked`. Gating on the source bytes would false-alarm
  constantly.
- **It is also the sole ownership record — no separate install-state DB.** The
  rpmdb/dpkg approach (a second database tracking what we installed) is a second
  source of truth that `remove`/`prune` must keep in sync. One file is simpler
  to reason about. Re-hashing a KB–MB skill on every status/sync is cheap, so
  the performance argument for a separate index doesn't apply (Cargo/pnpm/uv/Nix
  don't keep one either).

_Reversing either of these — pinning the commit instead of content, or adding an
install-state layer — would quietly break reproducibility or double the
ownership bookkeeping. Hence this note._

## Two-layer locking, and why `cache clean` must spare the lock files

Two things need concurrency protection at **different scopes**: one scope's
manifest/lock/skills_root (per-scope) and the shared content-addressed cache
(cross-scope — every scope reads the same `~/.cache/skm`). A single lock can't
serve both: a per-scope lock can't guard shared cache artifacts, and one global
lock would needlessly serialize unrelated projects. So:

- a **per-scope flock** guards one scope for the whole command, and
- the cache **`.cache.lock`** is the only cross-scope guard, held just for the
  materialization window.

The sharp edge: **`cache clean` must never delete `flocks/` or `.cache.lock`.**
Unlinking a live lock file removes its inode; a process that re-opens the same
path then gets a _fresh_ inode whose `flock` no longer mutually-excludes the
prior holder — silently allowing two writers on one scope. This rule lives
across `sys/cache.rs`, `cmd/cache_cmd.rs`, and `sys/fsutil.rs`, so it's recorded
here where a maintainer changing the cleanup logic will look before reasoning
about it.
