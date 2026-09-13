# Root cause: the parallel `apg node` / `apg edge` lock race

Change-set: `lbug-lock-race` (branch `lbug-lock-race`).
Phase: `lbug-lock-race/plan.phase-01` (reproduce, root-cause, select the fix — no fix
lands here). Reproduce with plan tasks:

- `plan.phase-01.task-4` — cross-process harness `apg.testutil.spawn_apg` (`src/testutil.rs`).
- `plan.phase-01.task-2` — flock-gap + RO/RW DB-open taxonomy
  (unit test `node_edge_entry_takes_no_spec_lock_and_db_open_taxonomy`, `src/node_cmd.rs`).
- `plan.phase-01.task-1` — the parallel burst
  (`#[ignore]` e2e test `parallel_node_edge_burst_is_serial_equivalent_with_per_lock_attribution`, `src/node_cmd.rs`).

> Filed under `plans/SPEC-*.md` because the code-writer edit grant covers
> `plans/SPEC*.md`, not a new `docs/` tree. It is a one-off phase-01 finding, not a
> product spec contract.

## Symptom

A burst of parallel `apg node` / `apg edge` mutations against one project worktree loses
roughly half its calls to lock errors, and a shared endpoint file can end up
pairing-inconsistent. Every loser exits non-zero; on the red tree the store is not the
serial application.

## Reproduction (task-1)

The repro spawns N separate `apg` CLI processes (not in-process calls, which share the
`SPEC_LOCK` `OnceLock` and would false-green) and asserts the store equals the serial
application. It has two stages.

Run:

```sh
cargo test parallel_node_edge_burst -- --ignored --nocapture
```

Stage A — DB present, the real project state. N = 10 separate processes each add one
`depends-on` edge from a shared `hub` to its own `leaf-i`:

```
stage A (db present): 9/10 failed; per-lock: {"lbug apg/.trans/db.lbug": 9}; hub out-edges: 1
```

Each loser:

```
apg: IO exception: Could not set lock on file : …/apg/.worktrees/foo/apg/.trans/db.lbug
(Error: Resource temporarily unavailable)
See the docs: https://docs.ladybugdb.com/concurrency for more information.
```

Serial application would leave the hub with **10** out-edges; the store has **1**.

Stage B — `db.lbug` removed, which isolates the non-DB half of the sequence (the node
path still writes the shared hub file and commits through `git::commit_files`):

```
stage B (no db): 9/10 failed; per-lock: {"git .git/index.lock": 1,
                                       "node-file RMW (pairing mismatch)": 8}; hub out-edges: 1
```

- the git loser: `apg: the index is locked; this might be due to a concurrent or crashed
  process; class=Index (10); code=Locked (-14)` — `.git/index.lock`, held by
  `git::commit_files` (`src/git.rs:634`, `index.write()`);
- the RMW losers: `node … in edge `depends-on` <- `…hub` has no matching out edge on
  `…hub` — an edge must appear in BOTH endpoint files` — one process's hub write clobbered
  another's, because the shared endpoint's read-modify-write is not serialized.

**The `specs.lock` flock never appears in either stage's per-lock attribution: it is
never acquired on the node/edge path.**

## The node/edge durable sequence and every lock on it

`apg node <sub>` / `apg edge <sub>` dispatch to `cmd_node` / `cmd_edge`
(`src/node_cmd.rs:54`, `:67`), then to `node_add`/`node_update`/`node_rm` or
`edge_add`/`edge_update`/`edge_rm`. Every write routes to `layers::write_project`
(`src/layers.rs:2042`):

1. `git::require_project_context` (`src/git.rs:514`) — membership guard, no lock.
2. staleness gate — no lock.
3. `validate_change` (`src/layers.rs:1798`) — when a DB exists, calls
   `artifacts::code_universes` (`src/layers.rs:1869`), which opens
   `ArtifactDb::open` **read-write** (`src/artifacts.rs:915` → `:232` →
   `Database::new(…, SystemConfig::default())` at `:240`). **This happens before any
   write.**
4. `write_through_with_deletes` (`src/layers.rs:1907`) — writes/removes the node files,
   then `git::commit_files` (`src/layers.rs:2005` → `src/git.rs:600`), whose
   `index.write()` at `src/git.rs:634` holds **`.git/index.lock`**; then
   `git::reanchor_scan_meta` (`src/layers.rs:2012` → `src/git.rs:689`), whose
   `Database::new` at `src/git.rs:720` is another **read-write** open.
5. post-commit re-merge (`src/layers.rs:2065/2067`) — `code_universes` again
   (`:2065`) and `reingest_layers` (`:2067` → `src/artifacts.rs:940`,
   `ArtifactDb::open` **read-write**).

So a single `apg node add` performs **three or four read-write `db.lbug` opens** and one
git index write. None of it is serialized against another process.

### The lock taxonomy: what fails vs what merely coexists

| class | open | site | contended over |
|---|---|---|---|
| read-write | `ArtifactDb::open` → `SystemConfig::default()` | `code_universes` `src/artifacts.rs:915`, `reingest_layers` `src/artifacts.rs:940` | **fails** across processes: `Could not set lock on file … Resource temporarily unavailable` |
| read-write | `Database::new(…, SystemConfig::default())` | `reanchor_scan_meta` `src/git.rs:720` | **fails** the same way |
| read-only | `Database::new(…, SystemConfig::default().read_only(true))` | `cmd_query` `src/main.rs:963` | **coexists** with a live read-write holder — it does *not* fail |
| git index | `repo.index()` / `index.write()` | `commit_files` `src/git.rs:634` | **fails**: `the index is locked … code=Locked (-14)` |
| flock | `libc::flock(…, LOCK_EX)` on `apg/.trans/specs.lock` | `acquire_spec_lock` `src/artifacts.rs:33` | **never reached on the node/edge path (the gap)** |

The read-write opens are the dominant loss (Stage A: 9/10). The read-only `apg query`
coexists with a live read-write handle, so it is not the failing class. The unlocked
node-file read-modify-write is not a lock at all, but it is a second race that survives
any DB fix (Stage B: 8/10).

## Why the existing flock misses the node/edge path

`apg.artifacts.acquire_spec_lock` (`src/artifacts.rs:33`) creates and `flock`s
`apg/.trans/specs.lock` and holds it for the process life; it is *reentrant* within a
process. It is a purpose-built serialization point, and it does gate the authoring
paths — its callers are the plan mutations (`plan_cmd.rs`) and the review mutations
(`review_cmd.rs`) only. It is **never called** by `cmd_node`, `cmd_edge`,
`node_add`/`edge_add`/… , `write_project`, `write_through_with_deletes`,
`commit_files`, or `reanchor_scan_meta`.

The consequence is directly observable: `node_edge_entry_takes_no_spec_lock_and_db_open_taxonomy`
(task-2) drives `cmd_node`/`cmd_edge` through the funnel and asserts that
`apg/.trans/specs.lock` is never created; the burst (task-1) records zero `specs.lock`
attributions. The lock exists but sits on a path the race never takes, so it cannot
serialize the node/edge durable sequence.

## Fix-ladder evaluation (task-3)

### (a) Extended whole-sequence file-level flock — **CHOSEN (the race fix)**

Extend `specs.lock` from "plan/review serialization" to the whole durable sequence:
acquire the single `LOCK_EX` flock **once** at the `cmd_node`/`cmd_edge` dispatch entry,
before any node-file read, and hold it across validate → write → commit → reingest.
Because the direct node/edge path and the coordinator share the same lock file, a burst
with no live session and a live session are mutually exclusive; process-wide-single-fd
reentrancy keeps a command that needs a second acquisition from deadlocking.

This is the only option that serializes *all three* stages the repro exposes — the
node-file RMW (Stage B pairing mismatch), `commit_files`'s `.git/index.lock` (Stage B),
and the read-write `db.lbug` opens (Stage A) — with one structural mechanism and no
speculative retries.

### (b) Bounded retry/backoff around a contended open — **REJECTED as primary**

Retrying a failed read-write open treats the symptom. It cannot repair the node-file
read-modify-write: a retry after a lost update re-reads a store that another process
already clobbered, and a retry after a pairing mismatch can duplicate or drop an edge.
It also degrades under load (all N contenders spin on the same lock), and it leaves the
race window open between the retry and the next commit. At most a guarded last resort,
never the fix.

### (c) DB-decoupling (validate from the node files / `graph.jsonl`, never open `db.lbug`
read-write) — **COMPLEMENT, not a standalone fix**

Decoupling removes the dominant Stage-A loss (the read-write `db.lbug` opens) and is
worth doing. But Stage B of the repro is exactly this world: with `db.lbug` gone the
node path still lost 9/10 — one to `.git/index.lock`, eight to the unlocked shared
read-modify-write. Decoupling alone leaves the node-file RMW and the git index commit
racing; it must ride *inside* (a).

### (d) Session-scoped single-writer coordinator — **MANDATORY**

A live coordinator owns the worktree DB, applies mutations in receive order, amortizes
one DB open/parse over N mutations, and applies each mutation's projection delta
write-through. It is mandatory because the transparent direct path can never amortize
and must stay safe on its own, while routed bursts need a single writer; it serializes
with (a) by holding the same flock. It is not a substitute for (a): sessions are
explicit, and the direct path must be correct without one.

### Chosen

**(a) extended whole-sequence flock** is the race fix; **(c) DB-decoupling** is the
complement that removes DB contention and keeps validation working without a read-write
open; **(d) the coordinator** is mandatory for amortization, write-through projection,
and routed single-writer mode. **(b)** is rejected as the primary fix.

## Evidence index

- `src/node_cmd.rs` — `node_edge_entry_takes_no_spec_lock_and_db_open_taxonomy`
  (task-2: flock gap; RO coexists, RW fails cross-process) and
  `parallel_node_edge_burst_is_serial_equivalent_with_per_lock_attribution`
  (task-1: per-lock attribution; `#[ignore]` until the phase-02 fix).
- `src/testutil.rs` — `apg_bin`, `ApgCommand`, `spawn_apg` (task-4).
- Lock sites: `src/artifacts.rs:33` (flock), `:915`/`:940` (read-write opens),
  `src/git.rs:634` (index write), `:720` (read-write open),
  `src/main.rs:963` (read-only open).
