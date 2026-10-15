// e2e boundary test for the curated suite tools: the stored<->absolute identity
// conversion (against a candidate `apg` binary), the git branch-delta
// requirement wrapper (git/fs only), and the V2 plugin/fs-scope surface (a
// faked opencode 2.x tool context + scratch /tmp repos). All are real I/O, so
// all are opt-in suite e2e.
//
// OPT-IN: real I/O (a scratch /tmp git repo, spawning `apg query`, git
// subprocesses). Plain `bun test` skips it; run it with
//
//   APG_SUITE_E2E=1 APG_BINARY="$PWD/target/debug/apg" \
//     bun test tests/boundary.e2e.test.ts
//
// Never pointed at a real project — only a throwaway /tmp repo
// (`global.constraint.no-real-project-test`).
import { test, expect } from "bun:test"
import { existsSync } from "node:fs"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"

import {
  runCypher,
  resolveProjectPath,
  findSymbolRebaseColumns,
  branchAddedRequirementNames,
  scopeProjectRequirements,
  fsScopeDecision,
  findApgRoot,
  mainCheckoutRoot,
  REQUIREMENT_FQN_PREFIX,
  csvToRows,
  startDetachedSession,
  sessionLiveAt,
  NO_DB_ERROR,
  type DetachedSession,
  type ToolContext,
} from "../lib/apg.ts"

// opencode-2x-support phase-02.task-41: the ported V2 plugin. Importing the
// tool module runs `Plugin.define({ id, setup })` at module scope, so the
// registration-shape e2e below can assert the returned plugin object directly.
import apgQuery from "../tools/apg_query.ts"

/// A faked opencode 2.x tool-executor context — the V2 shape
/// `{ agent, sessionID, messageID, id }` (no V1 `directory`/`worktree`) — plus
/// the plugin instance's load `location` (`ctx.location`, captured at `setup`),
/// whose `directory` seeds `findApgRoot` / `mainCheckoutRoot`. The scenarios
/// that pin an AUTHORITATIVE path pass it as the helpers' explicit `directory`
/// arg, so the location only needs to be present for the type.
function v2Context(dir: string, overrides: Partial<ToolContext> = {}): ToolContext {
  return {
    agent: "test-agent",
    sessionID: "s1",
    messageID: "m1",
    id: "c1",
    location: { directory: dir },
    ...overrides,
  }
}

/// The candidate binary: `APG_BINARY`, else the in-repo debug build. The suite
/// e2e is only meaningful against a binary that renders repo-relative
/// identities, so a missing candidate skips rather than asserting.
function candidateBinary(): string | null {
  const env = process.env.APG_BINARY
  if (env && existsSync(env)) return env
  const repoRoot = path.resolve(import.meta.dir, "..", "..")
  const debug = path.join(repoRoot, "target", "debug", "apg")
  return existsSync(debug) ? debug : null
}

const enabled = process.env.APG_SUITE_E2E === "1"
const binary = enabled ? candidateBinary() : null

/// A scratch git repo with one Go package, committed. Returns its root.
function scratchRepo(): string {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-boundary-"))
  const repo = path.join(base, "repo")
  fs.mkdirSync(path.join(repo, "pkg"), { recursive: true })
  fs.writeFileSync(
    path.join(repo, "pkg", "a.go"),
    "package pkg\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
  )
  // The Go frontend loads packages through `go/packages`, which needs a module;
  // without `go.mod` the scan emits no Go nodes (mirrors every Rust e2e scratch
  // Go repo, e.g. `tests/main_e2e.rs`).
  fs.writeFileSync(path.join(repo, "go.mod"), "module scratch\n\ngo 1.21\n")
  const run = (cmd: string[]) => Bun.spawnSync({ cmd, cwd: repo, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", "main"])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])
  return repo
}

/// A one-line requirement node-file body. `branchAddedRequirementNames` only
/// reads the file BASENAMES (the file name IS the requirement name), so the
/// JSON only needs to be plausible, not schema-valid.
function requirementNode(name: string): string {
  return `${JSON.stringify({ layer: "requirements", type: "requirement", name, body: `req ${name}`, feature: "test" })}\n`
}

/// A scratch git repo with an `apg/` layout: a dummy `apg/.trans/db.lbug` (so
/// `findApgRoot` resolves) and one PRE-EXISTING requirement node file,
/// committed on the DEFAULT branch; a LINKED worktree on a `feature` branch
/// then adds one NEW requirement node file and commits it. The main checkout
/// stays on the default branch — the real apg topology (project branches live
/// in linked worktrees) — so the default-branch NAME comes from the main
/// checkout's symbolic HEAD (`main_checkout_head`), never the feature branch
/// the delta is measured against. `defaultBranch` is parameterized so a
/// NON-`main` default (`master`) exercises the no-`origin/HEAD` fallback; no
/// remote is ever added, so `origin/HEAD` is always absent. Returns the main
/// checkout root and the feature worktree root (their common parent is the
/// throwaway base dir the caller removes).
function scratchRequirementRepo(defaultBranch = "main"): { repo: string; worktree: string } {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-reqdelta-"))
  const repo = path.join(base, "repo")
  const reqDir = path.join(repo, "apg", "layers", "requirements", "requirement")
  fs.mkdirSync(reqDir, { recursive: true })
  fs.mkdirSync(path.join(repo, "apg", ".trans"), { recursive: true })
  fs.writeFileSync(path.join(repo, "apg", ".trans", "db.lbug"), "")
  fs.writeFileSync(path.join(reqDir, "req-preexisting.json"), requirementNode("req-preexisting"))

  const run = (cmd: string[], cwd: string = repo) =>
    Bun.spawnSync({ cmd, cwd, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", defaultBranch])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])

  // The feature branch lives in a LINKED worktree; the main checkout stays on
  // the default branch.
  const worktree = path.join(base, "feature-wt")
  run(["git", "worktree", "add", "-q", "-b", "feature", worktree])
  const wtReqDir = path.join(worktree, "apg", "layers", "requirements", "requirement")
  fs.writeFileSync(path.join(wtReqDir, "req-branch-added.json"), requirementNode("req-branch-added"))
  run(["git", "add", "-A"], worktree)
  run(["git", "commit", "-q", "-m", "add requirement"], worktree)
  return { repo, worktree }
}

test.skipIf(!enabled || !binary)(
  "e2e: curated boundary rebases stored identities to absolute, raw apg query stays stored",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = bin

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to npm.
    fs.mkdirSync(path.join(home, ".opencode", "node_modules", "@opencode-ai", "plugin"), {
      recursive: true,
    })
    const env = { ...process.env, HOME: home }
    const run = (args: string[]) =>
      Bun.spawnSync({ cmd: [bin, ...args], cwd: repo, env, stdout: "pipe", stderr: "pipe" })

    try {
      const init = run(["init", "."])
      expect(init.exitCode).toBe(0)
      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) {
        throw new Error(`scan failed: ${scan.stderr.toString()}`)
      }

      const context = v2Context(repo)
      const query = "MATCH (f:File) RETURN f.fqn ORDER BY f.fqn"

      // RAW `apg query` is the graph view: the stored repo-relative form,
      // unchanged (no absolute checkout path).
      const raw = await runCypher(context, query, repo)
      expect(raw).toContain("pkg/a.go")
      expect(raw).not.toContain(repo)

      // A curated tool opts in to rebasing the File-fqn column: the stored
      // identity resolves to an existing absolute path under the caller's
      // project directory. The structural scanner also graphs the `init`
      // scaffold (`.gitignore`, `apg/config.json`) and `go.mod`, so select the
      // Go file's row rather than assuming row order.
      const rebased = await runCypher(context, query, repo, { rebaseColumns: [0] })
      const rows = csvToRows(rebased)
      const resolved = rows
        .slice(1)
        .map((r) => r[0])
        .find((v) => v.endsWith(path.join("pkg", "a.go")))
      expect(resolved).toBe(path.join(repo, "pkg/a.go"))
      const stored = resolveProjectPath(repo, resolved as string)
      expect(stored).toBe("pkg/a.go")
      expect(existsSync(resolved as string)).toBe(true)
      // The same stored identity resolves under a DIFFERENT checkout root to a
      // different absolute path (the two checkouts at one commit).
      const other = path.join(path.dirname(repo), "other-checkout")
      expect(resolveProjectPath(other, stored)).toBe(path.join(other, "pkg/a.go"))

      // An absolute input maps back to the stored value the query matched.
      expect(resolveProjectPath(repo, path.join(repo, "pkg/a.go"))).toBe(stored)

      // `apg_find_symbol`'s kind-aware boundary (feedback-17): its one query
      // returns a File row whose identity is the `n.fqn` cell (column 1) and
      // File nodes carry no `path`, so the old blind `rebaseColumns: [2]` was
      // a no-op and a consumer got the stored relative path. Rebasing with
      // `findSymbolRebaseColumns` resolves that fqn cell to an absolute path.
      const toolQuery =
        "MATCH (n) WHERE n.fqn CONTAINS 'a.go' AND labels(n) = 'File' " +
        "RETURN labels(n) as kind, n.fqn, n.path, n.start_line, n.end_line"
      const toolRows = csvToRows(
        await runCypher(context, toolQuery, repo, { rebaseColumns: findSymbolRebaseColumns }),
      )
      const fileRow = toolRows.slice(1).find((r) => r[0] === "File")
      expect(fileRow).toBeDefined()
      const fileFqn = (fileRow as string[])[1]
      expect(fileFqn).toBe(path.join(repo, "pkg/a.go"))
      expect(existsSync(fileFqn)).toBe(true)
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
)

// apg-plan-phases-scoping phase-03: the git branch-delta WRAPPER is real I/O
// (`Bun.$` git subprocesses) so it is covered here, at the opt-in e2e tier; the
// pure scoping core it feeds is unit-covered in `lib/apg.test.ts`. This wrapper
// needs only git + fs (no candidate `apg` binary), so it gates on the e2e opt-in
// alone.

test.skipIf(!enabled)(
  "e2e: git branch delta reports exactly the branch-added requirement names",
  async () => {
    const { repo, worktree } = scratchRequirementRepo()
    try {
      const context = v2Context(worktree)
      const added = await branchAddedRequirementNames(context, worktree)

      // Exactly the requirement node file added on this branch — not the
      // pre-existing one already carried by the default branch.
      expect([...added].sort()).toEqual(["req-branch-added"])
      expect(added.has("req-preexisting")).toBe(false)

      // The main checkout, on the default branch, sees an empty delta: no false
      // positives.
      const none = await branchAddedRequirementNames(v2Context(repo), repo)
      expect([...none]).toEqual([])
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: the git branch delta drives the project-scoped requirement decision",
  async () => {
    const { worktree } = scratchRequirementRepo()
    try {
      const context = v2Context(worktree)
      const branchAdded = await branchAddedRequirementNames(context, worktree)

      const req = (name: string) => `${REQUIREMENT_FQN_PREFIX}${name}`
      // `req-preexisting` was delivered by an earlier project (its transient
      // `Satisfies` edge died with its branch); THIS project's plan satisfies
      // nothing. Only the branch-added requirement is in scope, so the
      // earlier-delivered one is NOT reported unsatisfied.
      const scope = scopeProjectRequirements(
        [req("req-preexisting"), req("req-branch-added")],
        branchAdded,
        new Map(),
      )
      expect(scope.inScope).toEqual([req("req-branch-added")])
      expect(scope.unsatisfied).toEqual([req("req-branch-added")])
      expect(scope.overSatisfied).toEqual([])
    } finally {
      fs.rmSync(path.dirname(worktree), { recursive: true, force: true })
    }
  },
)

// apg-plan-phases-scoping phase-01.task-3 (feedback-4) / phase-03.task-3: a
// NON-`main` default branch with NO `origin/HEAD` must resolve its NAME from the
// MAIN checkout's symbolic HEAD — the binary's `main_checkout_head` fallback —
// not a hardcoded `main`. The branch delta must therefore report the
// branch-added requirement, not silently return empty.
test.skipIf(!enabled)(
  "e2e: a non-main default branch with no origin/HEAD resolves via the main checkout HEAD",
  async () => {
    const { repo, worktree } = scratchRequirementRepo("master")
    try {
      const context = v2Context(worktree)
      const added = await branchAddedRequirementNames(context, worktree)

      // The default resolved to the main checkout's `master`, so the
      // branch-added requirement IS reported — not a silent empty set from a
      // hardcoded `main` (which has no local ref here).
      expect([...added].sort()).toEqual(["req-branch-added"])
      expect(added.has("req-preexisting")).toBe(false)

      // Guard the premise: the main checkout is on `master` and `origin/HEAD`
      // is genuinely absent, so the main-checkout-HEAD fallback is what was
      // exercised (never the origin/HEAD branch).
      const mainHead = Bun.spawnSync({
        cmd: ["git", "-C", repo, "symbolic-ref", "--short", "HEAD"],
        stdout: "pipe",
        stderr: "pipe",
      })
      expect(mainHead.stdout.toString().trim()).toBe("master")
      const originHead = Bun.spawnSync({
        cmd: ["git", "-C", repo, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        stdout: "pipe",
        stderr: "pipe",
      })
      expect(originHead.exitCode).not.toBe(0)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

// opencode-2x-support phase-02.task-41: the V2 fs-scope gate. Phase 1 retired
// the V1 frontmatter-glob parser (`agentFsGlobs` / `editAllowGlobs`), so the fs
// tools (`apg_rm`/`apg_mv`/`apg_cp`) no longer re-derive an acting agent's edit
// grants here. They assert their resolved target path(s) through opencode 2.x's
// OWN permission engine — `ctx.permission.assert({ ..., action: "edit",
// resources })` — via the shared `fsScopeDecision`, and `throw new
// Error(reason)` when it refuses. The tool modules import `@opencode/plugin`
// (the V2 plugin dependency this suite installs); these scenarios stay
// plugin-free and drive the SAME sequence with a STUBBED `permission.assert`
// (a fake modelling opencode's resolve=allow / reject=deny) plus the real fs op
// gated on `decision.allowed`, against scratch /tmp git repos (no live opencode
// 2.x; `global.constraint.no-real-project-test`).

/// A scratch git repo with a `src/` tree and an out-of-scope `outside.txt`,
/// committed. Returns its root.
function scratchFsRepo(): string {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-fsscope-"))
  const repo = path.join(base, "repo")
  fs.mkdirSync(path.join(repo, "src"), { recursive: true })
  fs.writeFileSync(path.join(repo, "src", "a.rs"), "fn a() {}\n")
  fs.writeFileSync(path.join(repo, "src", "b.rs"), "fn b() {}\n")
  fs.writeFileSync(path.join(repo, "outside.txt"), "outside\n")
  const run = (cmd: string[]) => Bun.spawnSync({ cmd, cwd: repo, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", "main"])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])
  return repo
}

/** A stubbed `ctx.permission` modelling opencode's `edit` decision: `assert`
 *  resolves (allow) unless the single call's `resources` include one of
 *  `denied`, in which case it throws a denial naming that path. `calls` records
 *  each args object, so a test pins the ONE-call / both-endpoints contract. */
function stubPermission(denied: readonly string[] = []): {
  permission: { assert: (args: unknown) => Promise<void> }
  calls: unknown[]
} {
  const calls: unknown[] = []
  const permission = {
    assert: async (args: unknown): Promise<void> => {
      calls.push(args)
      const { resources } = args as { resources: readonly string[] }
      const hit = resources.find((r) => denied.includes(r))
      if (hit !== undefined) {
        throw new Error(`path is not within the acting agent's granted globs: ${hit}`)
      }
    },
  }
  return { permission, calls }
}

test.skipIf(!enabled)(
  "e2e: fsScopeDecision fail-closes with no permission surface — the gated unlink never runs",
  async () => {
    const repo = scratchFsRepo()
    try {
      const target = path.join(repo, "src", "a.rs")
      const decision = await fsScopeDecision(undefined, v2Context(repo), [target], [target])
      expect(decision.allowed).toBe(false)
      expect(decision.reason).toContain("opencode permission surface unavailable")
      if (decision.allowed) fs.unlinkSync(target)
      expect(existsSync(target)).toBe(true)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: fsScopeDecision gates a real unlink — allow-on-resolve removes, deny-on-throw refuses",
  async () => {
    const repo = scratchFsRepo()
    try {
      // allow: `assert` resolves, so the real op mutates the tree.
      const allowedFile = path.join(repo, "src", "a.rs")
      const allow = await fsScopeDecision(
        stubPermission().permission,
        v2Context(repo),
        [allowedFile],
        [allowedFile],
      )
      expect(allow.allowed).toBe(true)
      expect(allow.reason).toBe(null)
      if (allow.allowed) fs.unlinkSync(allowedFile)
      expect(existsSync(allowedFile)).toBe(false)

      // deny: `assert` throws, so the decision carries the reason and the gated
      // op never runs — the tree is untouched.
      const deniedFile = path.join(repo, "outside.txt")
      const deny = await fsScopeDecision(
        stubPermission([deniedFile]).permission,
        v2Context(repo),
        [deniedFile],
        [deniedFile],
      )
      expect(deny.allowed).toBe(false)
      expect(deny.reason).toContain("granted globs")
      if (deny.allowed) fs.unlinkSync(deniedFile)
      expect(existsSync(deniedFile)).toBe(true)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: apg_mv asserts BOTH endpoints in ONE call — a denied source or destination refuses the move",
  async () => {
    const repo = scratchFsRepo()
    try {
      // Both endpoints allowed: ONE assert call carries BOTH resolved paths, and
      // the gated rename runs.
      const from = path.join(repo, "src", "a.rs")
      const to = path.join(repo, "src", "a-moved.rs")
      const allowStub = stubPermission()
      const allow = await fsScopeDecision(
        allowStub.permission,
        v2Context(repo),
        [from, to],
        [from, to],
      )
      expect(allow.allowed).toBe(true)
      expect(allowStub.calls).toHaveLength(1)
      expect((allowStub.calls[0] as { resources: readonly string[] }).resources).toEqual([from, to])
      if (allow.allowed) fs.renameSync(from, to)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(from)).toBe(false)

      // A denied DESTINATION refuses the whole move: the source is allowed, but
      // the SAME single call carries the denied destination, so nothing moves.
      const deniedTo = path.join(repo, "outside-moved.rs")
      const dstStub = stubPermission([deniedTo])
      const dstDeny = await fsScopeDecision(
        dstStub.permission,
        v2Context(repo),
        [to, deniedTo],
        [to, deniedTo],
      )
      expect(dstDeny.allowed).toBe(false)
      expect(dstDeny.reason).toContain(deniedTo)
      expect(dstStub.calls).toHaveLength(1)
      if (dstDeny.allowed) fs.renameSync(to, deniedTo)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(deniedTo)).toBe(false)

      // A denied SOURCE is likewise refused.
      const deniedFrom = path.join(repo, "outside.txt")
      const srcDeny = await fsScopeDecision(
        stubPermission([deniedFrom]).permission,
        v2Context(repo),
        [deniedFrom, to],
        [deniedFrom, to],
      )
      expect(srcDeny.allowed).toBe(false)
      if (srcDeny.allowed) fs.renameSync(deniedFrom, to)
      expect(existsSync(deniedFrom)).toBe(true)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: apg_cp asserts BOTH endpoints in ONE call — a denied destination refuses the copy",
  async () => {
    const repo = scratchFsRepo()
    try {
      const from = path.join(repo, "src", "a.rs")
      const to = path.join(repo, "src", "a-copy.rs")
      const allowStub = stubPermission()
      const allow = await fsScopeDecision(
        allowStub.permission,
        v2Context(repo),
        [from, to],
        [from, to],
      )
      expect(allow.allowed).toBe(true)
      expect(allowStub.calls).toHaveLength(1)
      if (allow.allowed) fs.cpSync(from, to, { recursive: true })
      expect(existsSync(to)).toBe(true)
      expect(existsSync(from)).toBe(true) // the source survives a copy

      // A denied destination refuses the copy: nothing is written.
      const deniedTo = path.join(repo, "outside-copy.rs")
      const deny = await fsScopeDecision(
        stubPermission([deniedTo]).permission,
        v2Context(repo),
        [from, deniedTo],
        [from, deniedTo],
      )
      expect(deny.allowed).toBe(false)
      if (deny.allowed) fs.cpSync(from, deniedTo, { recursive: true })
      expect(existsSync(deniedTo)).toBe(false)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

// opencode-2x-support phase-02.task-41 (task-2/task-6 acceptance, flagged from
// phase-1): `findApgRoot` and `mainCheckoutRoot` source their walk-up START from
// the plugin load location (`ctx.location.directory`, then
// `ctx.location.project.directory`, then `ctx.location.project.canonical`, then
// `process.cwd()`) when no explicit `directory` arg is given, and never read the
// retired V1 `ctx.directory`/`ctx.worktree` fields. An explicit `directory` arg
// stays AUTHORITATIVE and wins over the location. Real I/O (fs + a git
// subprocess), so opt-in suite e2e.

/// A scratch git repo carrying an `apg/.trans/db.lbug` so `findApgRoot` resolves
/// it. The base is realpath'd (macOS `os.tmpdir()` sits behind a symlink) so
/// `mainCheckoutRoot`'s canonical root equals the returned path.
function scratchApgRootRepo(): string {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-apgroot-")))
  const repo = path.join(base, "repo")
  fs.mkdirSync(path.join(repo, "apg", ".trans"), { recursive: true })
  fs.mkdirSync(path.join(repo, "src"), { recursive: true })
  fs.writeFileSync(path.join(repo, "apg", ".trans", "db.lbug"), "")
  const run = (cmd: string[]) => Bun.spawnSync({ cmd, cwd: repo, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", "main"])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])
  return repo
}

test.skipIf(!enabled)(
  "e2e: findApgRoot/mainCheckoutRoot start from ctx.location; an explicit directory stays authoritative",
  async () => {
    const repo = scratchApgRootRepo()
    const other = scratchApgRootRepo()
    try {
      const nested = path.join(repo, "src")

      // findApgRoot with NO directory arg: the walk-up starts at ctx.location.
      expect(findApgRoot(v2Context(repo))).toBe(repo)
      expect(findApgRoot(v2Context(nested))).toBe(repo)
      // The project.location fallbacks are sourced too.
      expect(
        findApgRoot(v2Context(repo, { location: { project: { id: "p", directory: repo } } })),
      ).toBe(repo)
      expect(
        findApgRoot(v2Context(repo, { location: { project: { id: "p", canonical: repo } } })),
      ).toBe(repo)

      // An explicit directory wins over the location (here `other` also has a
      // root, so the explicit path is what decides).
      expect(findApgRoot(v2Context(other), repo)).toBe(repo)

      // mainCheckoutRoot with NO directory arg: the frame comes from
      // ctx.location.directory (canonicalised).
      expect(await mainCheckoutRoot(v2Context(repo))).toEqual({ root: repo, project: null })
      expect(await mainCheckoutRoot(v2Context(nested))).toEqual({ root: repo, project: null })
      // An explicit directory stays authoritative over the location.
      expect(await mainCheckoutRoot(v2Context(repo), other)).toEqual({ root: other, project: null })
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(path.dirname(other), { recursive: true, force: true })
    }
  },
)

// opencode-2x-support phase-02.task-41: the ported suite tools are V2
// `Plugin.define` plugins. Importing a tool module runs `Plugin.define({ id,
// setup })` at module scope; its `setup(ctx)` registers the tool through
// `ctx.tool.transform((editor) => editor.add({...}))`. Without a live opencode
// host this is pinned structurally: the plugin object carries the id + a setup
// function, a faked plugin context captures the added tool, and the tool's
// `execute` returns the `{ content }` result shape.
test.skipIf(!enabled)(
  "e2e: a ported tool registers through Plugin.define/ctx.tool.transform(editor.add) and executes to {content}",
  async () => {
    // The default export IS the `Plugin.define` result (identity id + setup).
    expect(apgQuery.id).toBe("apg_query")
    expect(typeof apgQuery.setup).toBe("function")

    // A faked plugin context: `ctx.tool.transform` runs the editor callback and
    // captures the tool added through `editor.add`.
    type AddedTool = {
      name?: string
      description?: string
      execute?: (input: unknown, context: ToolContext) => Promise<{ content: string }>
    }
    const added: AddedTool[] = []
    const editor = { add: (tool: AddedTool) => added.push(tool) }
    const pluginCtx = {
      location: { directory: os.tmpdir() },
      permission: { assert: async () => {} },
      tool: { transform: async (fn: (editor: typeof editor) => void) => void fn(editor) },
    }
    await apgQuery.setup(pluginCtx as never)

    expect(added).toHaveLength(1)
    const tool = added[0]
    expect(tool.name).toBe("apg_query")
    expect(typeof tool.description).toBe("string")
    expect(typeof tool.execute).toBe("function")

    // execute -> { content }: an explicit `directory` outside any apg project
    // resolves the shared NO_DB_ERROR string rather than throwing.
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-nodb-"))
    try {
      const result = await tool.execute!(
        { query: "MATCH (n) RETURN n", directory: outside },
        v2Context(outside),
      )
      expect(result).toEqual({ content: NO_DB_ERROR })
    } finally {
      fs.rmSync(outside, { recursive: true, force: true })
    }
  },
)

// apg-sessions phase-04.task-12: the `apg_session` suite tool. Its body is the
// plugin-free `startDetachedSession` helper (the tool wraps it and appends a
// status message), so — like the fs-tool scenarios above — this drives the SAME
// sequence the tool runs: spawn `apg session start` DETACHED, wait for its
// socket to answer live, and return while the coordinator keeps running. A
// durable `apg node` mutation then routes through the live session (buffered
// until `apg session save`, which flushes it in one commit), and `apg session
// end` releases the session. A second detached start while one is live, and a
// root with no `db.lbug`, both fail cleanly rather than blocking or throwing.
// Real I/O (scratch /tmp git repo, a scanned db.lbug, a detached process, a Unix
// socket, git commits), so opt-in suite e2e against a candidate binary
// (`global.constraint.no-real-project-test`).
test.skipIf(!enabled || !binary)(
  "e2e: apg_session starts a detached live session, routes a mutation, saves, and ends; no db fails cleanly",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    const previousHome = process.env.HOME
    process.env.APG_BINARY = bin

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-session-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to npm, and redirect HOME so the detached session never reads a real
    // `~/.opencode` (`global.constraint.no-real-project-test`).
    fs.mkdirSync(path.join(home, ".opencode", "node_modules", "@opencode-ai", "plugin"), {
      recursive: true,
    })
    process.env.HOME = home

    const run = (args: string[]) =>
      Bun.spawnSync({ cmd: [bin, ...args], cwd: repo, stdout: "pipe", stderr: "pipe" })

    let live: DetachedSession | null = null
    try {
      expect(run(["init", "."]).exitCode).toBe(0)
      // Commit the scaffold (the documented scratch flow) so the session's own
      // save commit lands on a clean tracked tree.
      const git = (args: string[]) =>
        Bun.spawnSync({ cmd: ["git", ...args], cwd: repo, stdout: "pipe", stderr: "pipe" })
      git(["add", "-A"])
      git(["commit", "-q", "-m", "apg init"])

      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) throw new Error(`scan failed: ${scan.stderr.toString()}`)

      const context = v2Context(repo)

      // The tool's plugin-free core: detached `apg session start`, returning
      // once the socket answers live while the coordinator keeps running.
      const started = await startDetachedSession(context, repo, { timeoutMs: 30000 })
      expect(typeof started).not.toBe("string")
      live = started as DetachedSession
      expect(live.root).toBe(repo)
      expect(existsSync(live.socket)).toBe(true)
      expect(await sessionLiveAt(live.socket)).toBe(true)

      // A routed durable mutation is admitted while the session is live, and
      // stays buffered (no node file on disk) until save.
      const nodeFile = path.join(
        repo,
        "apg",
        "layers",
        "requirements",
        "requirement",
        "session-node.json",
      )
      const added = run([
        "node",
        "add",
        "requirements",
        "requirement",
        "session-node",
        "--body",
        "routed",
      ])
      expect(added.exitCode).toBe(0)
      expect(added.stdout.toString().trim()).toBe(
        "Added node requirements.requirement.session-node",
      )
      expect(existsSync(nodeFile)).toBe(false)

      // A SECOND detached start while one is live fails cleanly: `apg session
      // start` exits early (one session per worktree DB), so no socket ever
      // appears and the helper reports an error string instead of blocking.
      const second = await startDetachedSession(context, repo, { timeoutMs: 30000 })
      expect(typeof second).toBe("string")
      expect(second as string).toContain("apg session start failed")

      // `apg session save` flushes the whole buffer in one commit …
      const save = run(["session", "save"])
      expect(save.exitCode).toBe(0)
      expect(save.stdout.toString().trim()).toBe("Session saved")
      expect(existsSync(nodeFile)).toBe(true)

      // … and `apg session end` releases the live session.
      const ended = run(["session", "end"])
      expect(ended.exitCode).toBe(0)
      expect(ended.stdout.toString().trim()).toBe("Session ended")
      // The session is released: its socket is reclaimed. (Poll briefly: the
      // coordinator removes the socket just after replying, and `sessionLiveAt`
      // is for a socket file that EXISTS with no listener — connecting to a
      // removed path does not settle, so assert on the file itself.)
      const gone = Date.now() + 10000
      while (existsSync(live.socket) && Date.now() < gone) {
        await new Promise((r) => setTimeout(r, 50))
      }
      expect(existsSync(live.socket)).toBe(false)
      live = null

      // Failure path: a root with no `db.lbug` (a bare dir, then an `apg/.trans`
      // with no db) fails cleanly with the shared NO_DB_ERROR — the socket can
      // never appear, and nothing is spawned or thrown.
      const noDb = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-session-nodb-"))
      try {
        const bare = path.join(noDb, "bare")
        fs.mkdirSync(bare, { recursive: true })
        expect(await startDetachedSession(v2Context(bare), bare)).toBe(NO_DB_ERROR)
        const badRoot = path.join(noDb, "bad")
        fs.mkdirSync(path.join(badRoot, "apg", ".trans"), { recursive: true })
        expect(await startDetachedSession(v2Context(badRoot), badRoot)).toBe(NO_DB_ERROR)
      } finally {
        fs.rmSync(noDb, { recursive: true, force: true })
      }
    } finally {
      // Best-effort teardown: end a still-live session (or kill it if the end
      // never reaches it) so no detached coordinator outlives the test.
      try {
        run(["session", "end"])
      } catch {}
      if (live) {
        try {
          process.kill(live.pid)
        } catch {}
      }
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      if (previousHome === undefined) delete process.env.HOME
      else process.env.HOME = previousHome
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
  60000,
)

// apg-sessions phase-04.task-18: regression guard for the hang-proof liveness
// probe (task-17). `sessionLiveAt` must resolve `false` — never hang — for a
// socket path that does not exist, for a path that once existed and was removed,
// and for a path where a non-socket file sits with no listener behind it (the
// removed/stale socket shapes a crashed coordinator leaves behind). The probe's
// hard, socket-event-independent deadline is what bounds the wait; a settled
// promise proves the hang regression stays fixed. `startDetachedSession` must
// likewise return its string error (not hang) for a root with no live session.
// Real I/O (Unix-socket connects, fs), so opt-in suite e2e.
test.skipIf(!enabled)(
  "e2e: sessionLiveAt resolves false without hanging for absent/removed socket paths",
  async () => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-session-probe-"))
    try {
      // (1) A path that was never bound.
      const missing = path.join(base, "never-bound.sock")
      const t0 = Date.now()
      expect(await sessionLiveAt(missing, 500)).toBe(false)
      expect(Date.now() - t0).toBeLessThan(5000)

      // (2) A path that once existed and was removed (the crash shape: the
      // coordinator's socket file is gone but a client still probes the path).
      const removed = path.join(base, "removed.sock")
      fs.writeFileSync(removed, "")
      fs.rmSync(removed, { force: true })
      expect(existsSync(removed)).toBe(false)
      const t1 = Date.now()
      expect(await sessionLiveAt(removed, 500)).toBe(false)
      expect(Date.now() - t1).toBeLessThan(5000)

      // (3) A stale socket FILE with no listener behind it (present-but-
      // unreachable): a regular file at the socket path.
      const stale = path.join(base, "stale.sock")
      fs.writeFileSync(stale, "")
      expect(existsSync(stale)).toBe(true)
      const t2 = Date.now()
      expect(await sessionLiveAt(stale, 500)).toBe(false)
      expect(Date.now() - t2).toBeLessThan(5000)

      // (4) `startDetachedSession` reports a string error (not a hang) for a
      // root with no live session — a bare dir with no apg root resolves the
      // shared NO_DB_ERROR promptly rather than blocking.
      const bare = path.join(base, "bare")
      fs.mkdirSync(bare, { recursive: true })
      const t3 = Date.now()
      const started = await startDetachedSession(v2Context(bare), bare, { timeoutMs: 500 })
      expect(typeof started).toBe("string")
      expect(started as string).toBe(NO_DB_ERROR)
      expect(Date.now() - t3).toBeLessThan(5000)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
  15000,
)
