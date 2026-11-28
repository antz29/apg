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
  runCli,
  runInProject,
  resolveProjectPath,
  resolveProjectWorktree,
  findSymbolRebaseColumns,
  branchAddedRequirementNames,
  scopeProjectRequirements,
  fsScopeDecision,
  mainCheckoutRoot,
  REQUIREMENT_FQN_PREFIX,
  csvToRows,
  MISSING_WORKTREE_ERROR,
  NO_DB_ERROR,
  QUERY_FAILED_PREFIX,
  type ToolContext,
} from "../lib/apg.ts"

// The ported V2 plugin. Importing the tool module runs
// `Plugin.define({ id, setup })` at module scope, so the registration-shape e2e
// below can assert the returned plugin object directly.
import apgQuery from "../tools/apg_query.ts"
import apgScan from "../tools/apg_scan.ts"
import apgProject from "../tools/apg_project.ts"

/// A faked opencode 2.x tool-executor context — the V2 shape
/// `{ agent, sessionID, messageID, id }` (no V1 `directory`/`worktree`) — plus
/// the plugin instance's load `location` (`ctx.location`, captured at `setup`),
/// whose `directory` seeds `mainCheckoutRoot` when no project is resolved. A
/// project-bound scenario adds the resolved `worktree` (and `projectName`),
/// which is then the sole spawn anchor.
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

/// A scratch git repo with an `apg/` layout: a dummy `apg/.trans/db.lbug` and
/// one PRE-EXISTING requirement node file,
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
  "e2e: a required project resolves through the registry and rebases stored identities against its worktree",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = bin

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to `bun install`.
    fs.mkdirSync(path.join(home, ".config", "opencode", "node_modules", "@opencode", "plugin"), {
      recursive: true,
    })
    const env = { ...process.env, HOME: home, APG_CALLER_SESSION: "s1" }
    const project = "suite-boundary"
    const worktreeRaw = path.join(repo, "apg", ".worktrees", project)
    try {
      const run = (args: string[], cwd: string = repo) =>
        Bun.spawnSync({ cmd: [bin, ...args], cwd, env, stdout: "pipe", stderr: "pipe" })
      const git = (args: string[]) =>
        Bun.spawnSync({ cmd: ["git", ...args], cwd: repo, stdout: "pipe", stderr: "pipe" })

      expect(run(["init", "."]).exitCode).toBe(0)
      // Commit the scaffold so the recorded scan matches the checkout state.
      git(["add", "-A"])
      git(["commit", "-q", "-m", "apg init"])
      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) {
        throw new Error(`scan failed: ${scan.stderr.toString()}`)
      }
      const started = run(["project", "start", project])
      if (started.exitCode !== 0) {
        throw new Error(`project start failed: ${started.stderr.toString()}`)
      }
      // The registry stores the canonical worktree path.
      const worktree = fs.realpathSync(worktreeRaw)

      // The caller sits in the MAIN checkout; the required project name
      // resolves through the registry to the project's own worktree.
      const context = v2Context(repo)
      expect(await resolveProjectWorktree(context, project)).toBe(worktree)

      // An unregistered name is refused by the binary, naming the registry path
      // and the registered names — never a fabricated path or a cwd fallback.
      let refusal = ""
      try {
        await resolveProjectWorktree(context, "no-such-project")
      } catch (e) {
        refusal = e instanceof Error ? e.message : String(e)
      }
      expect(refusal).toContain("not registered")
      expect(refusal).toContain("projects.db")
      expect(refusal).toContain("main")
      expect(refusal).toContain(project)
      // The refusal names the command it ran, that command's exit code and the
      // cwd the lookup ran against — the caller's context directory chain,
      // since no resolved worktree exists yet.
      const directRefusal = Bun.spawnSync({
        cmd: [bin, "project", "resolve", "no-such-project"],
        cwd: repo,
        stdout: "pipe",
        stderr: "pipe",
      })
      expect(directRefusal.exitCode).not.toBe(0)
      expect(refusal).toContain("apg project resolve no-such-project")
      expect(refusal).toContain(`failed (exit ${directRefusal.exitCode})`)
      expect(refusal).toContain(`cwd: ${repo}`)

      // A project-bound context: the caller's load location stays the MAIN
      // checkout; the resolved worktree is what every spawn runs in.
      const bound: ToolContext = { ...v2Context(repo), projectName: project, worktree }

      // The fs-scope frame is the MAIN checkout, decoded from the resolved
      // worktree alone.
      expect((await mainCheckoutRoot(bound))?.root).toBe(fs.realpathSync(repo))
      expect((await mainCheckoutRoot(bound))?.project).toBe(project)

      const query = "MATCH (f:File) RETURN f.fqn ORDER BY f.fqn"
      // RAW `apg query` is the graph view: the stored repo-relative form.
      const raw = await runCypher(bound, query)
      expect(raw).toContain("pkg/a.go")
      expect(raw).not.toContain(worktree)

      // A curated tool rebases the File-fqn column: the stored identity resolves
      // to an absolute path under the RESOLVED WORKTREE, not the caller's cwd.
      const rebased = await runCypher(bound, query, { rebaseColumns: [0] })
      const rows = csvToRows(rebased)
      const resolved = rows
        .slice(1)
        .map((r) => r[0])
        .find((v) => v.endsWith(path.join("pkg", "a.go")))
      expect(resolved).toBe(path.join(worktree, "pkg", "a.go"))
      expect(existsSync(resolved as string)).toBe(true)
      expect(resolveProjectPath(worktree, resolved as string)).toBe("pkg/a.go")
      // The caller's own checkout has no such absolute path: the rebase is
      // anchored on the resolved worktree, never on `context.location`.
      expect(resolved).not.toBe(path.join(repo, "pkg", "a.go"))

      // `runInProject` runs in the resolved worktree with the caller's project
      // environment set, leaving the caller's own cwd untouched.
      const cwdBefore = process.cwd()
      expect(await runInProject(bound, ["project", "resolve", project])).toBe(worktree)
      expect(process.cwd()).toBe(cwdBefore)

      // An INDUCED non-zero exit names the root/worktree the command actually
      // ran against, alongside the command, its exit code and its stderr — so
      // an operator reading a suite error knows which checkout produced it.
      const cliFailure = await runCli(bound, ["definitely-not-a-subcommand"])
      expect(typeof cliFailure).toBe("string")
      expect(cliFailure).toContain("definitely-not-a-subcommand")
      expect(cliFailure).toContain("failed (exit ")
      expect(cliFailure).toContain(`root: ${worktree}`)
      expect(cliFailure).toContain("unknown subcommand")

      const projectFailure = await runInProject(bound, ["definitely-not-a-subcommand"])
      expect(typeof projectFailure).toBe("string")
      expect(projectFailure).toContain("definitely-not-a-subcommand")
      expect(projectFailure).toContain("failed (exit ")
      expect(projectFailure).toContain(`worktree: ${worktree}`)
      expect(projectFailure).toContain("unknown subcommand")

      // An invalid Cypher still returns the QUERY_FAILED_PREFIX payload, with
      // the resolved worktree appended after it.
      const queryFailure = await runCypher(bound, "MATCH (")
      expect(queryFailure.startsWith(QUERY_FAILED_PREFIX)).toBe(true)
      expect(queryFailure).toContain(worktree)

      // `apg_find_symbol`'s kind-aware boundary: its one query returns a File
      // row whose identity is the `n.fqn` cell (column 1) and File nodes carry
      // no `path`, so `findSymbolRebaseColumns` resolves that fqn cell to an
      // absolute path under the resolved worktree.
      const toolQuery =
        "MATCH (n) WHERE n.fqn CONTAINS 'a.go' AND labels(n) = 'File' " +
        "RETURN labels(n) as kind, n.fqn, n.path, n.start_line, n.end_line"
      const toolRows = csvToRows(
        await runCypher(bound, toolQuery, { rebaseColumns: findSymbolRebaseColumns }),
      )
      const fileRow = toolRows.slice(1).find((r) => r[0] === "File")
      expect(fileRow).toBeDefined()
      expect((fileRow as string[])[1]).toBe(path.join(worktree, "pkg", "a.go"))
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
)

// `apg_project`'s `start` is a main-checkout operation whose registry row is
// owned by the session `runCli` puts on its child env. The DISCRIMINATION PIN:
// this process starts with NO `APG_CALLER_SESSION` (deleted below, restored in
// finally), so no inherited `s1` can own the row — without runCli's own
// injection the row stays ownerless and the project-bound call below is
// refused. The POSITIVE half proves the opposite: the same session then makes a
// bound call with no claim round-trip. The NEGATIVE half, after the positive,
// rebuilds the live rows ownerless (`apg project reconstruct`) and the SAME
// bound call is refused with the binary's claim-required message. Real I/O
// (scratch /tmp repo, a real scan, a project worktree and registry), so opt-in
// suite e2e against a candidate binary
// (`global.constraint.no-real-project-test`).
test.skipIf(!enabled || !binary)(
  "e2e: apg_project start records runCli's injected caller session as the registry owner",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    const previousSession = process.env.APG_CALLER_SESSION
    process.env.APG_BINARY = bin
    // Discrimination pin: no ambient session may own the registry row — only
    // the env `runCli` injects on the start child may.
    delete process.env.APG_CALLER_SESSION

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-project-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to `bun install`.
    fs.mkdirSync(path.join(home, ".config", "opencode", "node_modules", "@opencode", "plugin"), {
      recursive: true,
    })
    const env = { ...process.env, HOME: home }
    const project = "suite-root-plumbing"
    try {
      const run = (args: string[], cwd: string = repo) =>
        Bun.spawnSync({ cmd: [bin, ...args], cwd, env, stdout: "pipe", stderr: "pipe" })
      const git = (args: string[]) =>
        Bun.spawnSync({ cmd: ["git", ...args], cwd: repo, stdout: "pipe", stderr: "pipe" })

      expect(run(["init", "."]).exitCode).toBe(0)
      // Commit the scaffold so the recorded scan matches the checkout state and
      // `project start`'s clean-default gate passes.
      git(["add", "-A"])
      git(["commit", "-q", "-m", "apg init"])
      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) throw new Error(`scan failed: ${scan.stderr.toString()}`)

      // A faked plugin context: capture the tool added through `editor.add`.
      type AddedTool = {
        name?: string
        execute?: (input: unknown, context: ToolContext) => Promise<{ content: string }>
      }
      const added: AddedTool[] = []
      const editor = { add: (tool: AddedTool) => added.push(tool) }
      await apgProject.setup({
        location: { directory: repo },
        permission: { assert: async () => {} },
        tool: { transform: async (fn: (e: typeof editor) => void) => void fn(editor) },
      } as never)
      expect(added).toHaveLength(1)
      expect(added[0].name).toBe("apg_project")

      // The tool's `start` action runs from the main checkout and spawns through
      // `runCli`, whose child env names the caller session — the ONLY owner
      // source on this spawn (the ambient variable was deleted above).
      const started = await added[0].execute!({ project, action: "start" }, v2Context(repo))
      expect(started.content).toContain(`Project \`${project}\` started at`)
      expect(started.content).not.toContain("refused")

      const worktree = fs.realpathSync(path.join(repo, "apg", ".worktrees", project))
      const bound: ToolContext = {
        ...v2Context(repo, { sessionID: "s1" }),
        projectName: project,
        worktree,
      }

      // POSITIVE: the row is owned by `s1`, so the same session's project-bound
      // query succeeds — no claim round-trip. Without runCli's injection the row
      // would be ownerless and this would already be the claim-required
      // refusal.
      const owned = await runInProject(bound, ["query", "MATCH (n) RETURN count(*)"])
      expect(owned).not.toContain("failed (exit")
      expect(owned).not.toContain("registered with no owner")
      expect(owned).not.toContain("apg project claim")
      const ownedLines = owned.split("\n").filter((line) => line.length > 0)
      expect(ownedLines.length).toBeGreaterThanOrEqual(2)
      expect(Number(ownedLines[1])).toBeGreaterThan(0)

      // NEGATIVE (after the positive): reconstruct rebuilds every live row with
      // no owner, so the SAME session's bound call is refused with the binary's
      // claim-required message — the no-claim claim is demonstrable.
      const rebuilt = run(["project", "reconstruct"])
      if (rebuilt.exitCode !== 0) {
        throw new Error(`project reconstruct failed: ${rebuilt.stderr.toString()}`)
      }
      const refused = await runInProject(bound, ["query", "MATCH (n) RETURN count(*)"])
      expect(refused).toContain(`project \`${project}\``)
      expect(refused).toContain("registered with no owner")
      expect(refused).toContain("until it is claimed")
      expect(refused).toContain(`Fix: take ownership with \`apg project claim ${project}\``)
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      if (previousSession === undefined) delete process.env.APG_CALLER_SESSION
      else process.env.APG_CALLER_SESSION = previousSession
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
  60000,
)

// The git branch-delta WRAPPER is real I/O (`Bun.$` git subprocesses) so it is
// covered here, at the opt-in e2e tier; the pure scoping core it feeds is
// unit-covered in `lib/apg.test.ts`. This wrapper needs only git + fs (no
// candidate `apg` binary), so it gates on the e2e opt-in alone.

test.skipIf(!enabled)(
  "e2e: git branch delta reports exactly the branch-added requirement names",
  async () => {
    const { repo, worktree } = scratchRequirementRepo()
    try {
      const context = v2Context(worktree, { worktree })
      const added = await branchAddedRequirementNames(context)

      // Exactly the requirement node file added on this branch — not the
      // pre-existing one already carried by the default branch.
      expect([...added].sort()).toEqual(["req-branch-added"])
      expect(added.has("req-preexisting")).toBe(false)

      // The main checkout, on the default branch, sees an empty delta: no false
      // positives.
      const none = await branchAddedRequirementNames(v2Context(repo, { worktree: repo }))
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
      const context = v2Context(worktree, { worktree })
      const branchAdded = await branchAddedRequirementNames(context)

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

// A NON-`main` default branch with NO `origin/HEAD` must resolve its NAME from
// the MAIN checkout's symbolic HEAD — the binary's `main_checkout_head`
// fallback — not a hardcoded `main`. The branch delta must therefore report the
// branch-added requirement, not silently return empty.
test.skipIf(!enabled)(
  "e2e: a non-main default branch with no origin/HEAD resolves via the main checkout HEAD",
  async () => {
    const { repo, worktree } = scratchRequirementRepo("master")
    try {
      const context = v2Context(worktree, { worktree })
      const added = await branchAddedRequirementNames(context)

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

// The V2 fs-scope gate. The V1 frontmatter-glob parser (`agentFsGlobs` /
// `editAllowGlobs`) is retired, so the fs tools (`apg_rm`/`apg_mv`/`apg_cp`) no
// longer re-derive an acting agent's edit grants here. They assert their
// resolved target path(s) through opencode 2.x's OWN permission engine —
// `ctx.permission.assert({ ..., action: "edit", resources })` — via the shared
// `fsScopeDecision`, and `throw new Error(reason)` when it refuses. The tool
// modules import `@opencode/plugin` (the V2 plugin dependency this suite
// installs); these scenarios stay plugin-free and drive the SAME sequence with
// a STUBBED `permission.assert` (a fake modelling opencode's resolve=allow /
// reject=deny) plus the real fs op gated on `decision.allowed`, against scratch
// /tmp git repos (no live opencode 2.x;
// `global.constraint.no-real-project-test`).

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

/// A scratch main checkout carrying a REAL linked git worktree at
/// `<repo>/apg/.worktrees/demo` — the documented layout `mainCheckoutRoot`
/// decodes first, so the frame root is the main checkout while the directory
/// being processed is the worktree — plus a denied DESTINATION inside the main
/// checkout (never created: it is a destination) and a path genuinely outside
/// the checkout. Every path is returned raw AND canonical: macOS `os.tmpdir()`
/// sits behind the `/var` → `/private/var` symlink, so the frame root the
/// resolver returns is the `realpathSync` spelling, never the `mkdtemp` string.
/// Callers clean up `path.dirname(repo)` in `finally`, like `scratchFsRepo`.
function scratchWorktreeFsRepo(): {
  repo: string
  repoReal: string
  worktree: string
  worktreeReal: string
  file: string
  fileReal: string
  deniedDestination: string
  deniedDestinationReal: string
  outside: string
  outsideReal: string
} {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-worktree-"))
  const repo = path.join(base, "repo")
  const worktree = path.join(repo, "apg", ".worktrees", "demo")
  fs.mkdirSync(path.join(repo, "src"), { recursive: true })
  fs.writeFileSync(path.join(repo, "src", "a.rs"), "fn a() {}\n")
  fs.writeFileSync(path.join(repo, "src", "b.rs"), "fn b() {}\n")

  // A silently broken worktree would make the worktree scenarios assert nothing,
  // so every git step is checked and a failure rethrows the captured stderr.
  const git = (cmd: string[], cwd: string) => {
    const out = Bun.spawnSync({ cmd: ["git", ...cmd], cwd, stdout: "pipe", stderr: "pipe" })
    if (out.exitCode !== 0) {
      throw new Error(`git ${cmd.join(" ")} failed: ${out.stderr.toString()}`)
    }
  }
  git(["init", "-q", "-b", "main"], repo)
  git(["config", "user.email", "apg@localhost"], repo)
  git(["config", "user.name", "apg"], repo)
  git(["add", "-A"], repo)
  git(["commit", "-q", "-m", "init"], repo)
  fs.mkdirSync(path.join(repo, "apg", ".worktrees"), { recursive: true })
  git(["worktree", "add", "-b", "demo", worktree, "HEAD"], repo)

  const file = path.join(worktree, "scratch.txt")
  fs.writeFileSync(file, "scratch\n")
  const outside = path.join(base, "outside.txt")
  fs.writeFileSync(outside, "outside\n")
  const deniedDestination = path.join(repo, "denied", "scratch-moved.txt")

  const repoReal = fs.realpathSync(repo)
  return {
    repo,
    repoReal,
    worktree,
    worktreeReal: fs.realpathSync(worktree),
    file,
    fileReal: fs.realpathSync(file),
    deniedDestination,
    // Nothing exists yet below `<repo>`, so the deepest EXISTING ancestor
    // (`repoReal`) carries the canonical prefix and the destination tail is
    // re-appended lexically — the rule `canonicalPath` applies to a non-existent
    // destination.
    deniedDestinationReal: path.join(repoReal, "denied", path.basename(deniedDestination)),
    outside,
    outsideReal: fs.realpathSync(outside),
  }
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
    // macOS `os.tmpdir()` sits behind the `/var` → `/private/var` symlink, and
    // the decision's frame root is the canonical spelling: every path the case
    // asserts is derived from the canonical repo, never the raw `mkdtemp` string.
    const repo = fs.realpathSync(scratchFsRepo())
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
      // op never runs — the tree is untouched. The stub keys on the
      // MAIN-ANCHORED resource the decision asserts (`outside.txt`), never the
      // absolute path: `stubPermission`'s exact-membership check compares
      // verbatim, so an absolute key would silently ALLOW.
      const deniedFile = path.join(repo, "outside.txt")
      const denyStub = stubPermission(["outside.txt"])
      const deny = await fsScopeDecision(
        denyStub.permission,
        v2Context(repo),
        [deniedFile],
        [deniedFile],
      )
      expect(deny.allowed).toBe(false)
      expect(deny.reason).toContain("granted globs")
      expect((denyStub.calls[0] as { resources: readonly string[] }).resources).toEqual([
        "outside.txt",
      ])
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
    // Canonical frame spelling: every asserted value below is BARE
    // main-relative, derived from the root the decision resolves.
    const repo = fs.realpathSync(scratchFsRepo())
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
      expect((allowStub.calls[0] as { resources: readonly string[] }).resources).toEqual([
        "src/a.rs",
        "src/a-moved.rs",
      ])
      if (allow.allowed) fs.renameSync(from, to)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(from)).toBe(false)

      // A denied DESTINATION refuses the whole move: the source is allowed, but
      // the SAME single call carries the denied destination, so nothing moves.
      // Both the deny key and the reason pin use the anchored spelling.
      const deniedTo = path.join(repo, "outside-moved.rs")
      const dstStub = stubPermission(["outside-moved.rs"])
      const dstDeny = await fsScopeDecision(
        dstStub.permission,
        v2Context(repo),
        [to, deniedTo],
        [to, deniedTo],
      )
      expect(dstDeny.allowed).toBe(false)
      expect(dstDeny.reason).toContain("outside-moved.rs")
      expect(dstStub.calls).toHaveLength(1)
      if (dstDeny.allowed) fs.renameSync(to, deniedTo)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(deniedTo)).toBe(false)

      // A denied SOURCE is likewise refused.
      const deniedFrom = path.join(repo, "outside.txt")
      const srcDeny = await fsScopeDecision(
        stubPermission(["outside.txt"]).permission,
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
    const repo = fs.realpathSync(scratchFsRepo())
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

      // A denied destination refuses the copy: nothing is written. The stub keys
      // on the anchored destination (`outside-copy.rs`), never the absolute path.
      const deniedTo = path.join(repo, "outside-copy.rs")
      const deny = await fsScopeDecision(
        stubPermission(["outside-copy.rs"]).permission,
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

// The WORKTREE frame: the acting agent's `edit` grants are written against the
// MAIN checkout (a code-writer's grants are `apg/.worktrees/*/<glob>`), while
// the directory being processed is the linked worktree. `fsScopeDecision`
// therefore resolves the frame root from `context.worktree` ALONE and asserts
// BARE main-relative resources — the form those grants match — never the
// absolute paths that would re-trigger the opening `*` deny. Every expectation
// below is derived from the fixture's canonical `realpathSync` spellings: macOS
// `os.tmpdir()` sits behind the `/var` → `/private/var` symlink, so the raw
// `mkdtemp` string is NOT the frame root.
async function worktreeScopePermission(): Promise<void> {
  const { repo, repoReal, worktree, file, fileReal, deniedDestination, deniedDestinationReal, outside, outsideReal } =
    scratchWorktreeFsRepo()
  // The bare main-relative resource the decision asserts for a canonical path.
  const anchored = (p: string) => path.relative(repoReal, p).split(path.sep).join("/")
  // The load location is deliberately NOT the worktree: the frame is anchored
  // on `context.worktree` alone, so a location that would resolve elsewhere
  // cannot change it.
  const ctx: ToolContext = { ...v2Context(os.tmpdir()), worktree }
  try {
    // The frame root is the MAIN checkout, decoded from the documented
    // `<repo>/apg/.worktrees/demo` layout the resolved worktree occupies — not
    // the worktree being processed.
    const root = await mainCheckoutRoot(ctx)
    expect(root).not.toBeNull()
    expect(root?.root).toBe(repoReal)
    expect(root?.project).toBe("demo")

    // Create-then-delete round trip through apg_rm's decision. The stub denies
    // the ABSOLUTE spelling, so an allow proves the asserted resource is the
    // bare main-relative form — and the pinned value names that form exactly.
    const rmStub = stubPermission([fileReal])
    const rm = await fsScopeDecision(rmStub.permission, ctx, [file], [file])
    expect(rm.allowed).toBe(true)
    expect(rm.reason).toBe(null)
    expect(rmStub.calls).toHaveLength(1)
    expect((rmStub.calls[0] as { resources: readonly string[] }).resources).toEqual([
      "apg/.worktrees/demo/scratch.txt",
    ])
    if (rm.allowed) fs.unlinkSync(file)
    expect(existsSync(file)).toBe(false)

    // A denied mv/cp DESTINATION refuses the whole operation: both endpoints
    // ride ONE assert call, so the call that allows the worktree source also
    // carries the denied destination (anchored) and nothing moves.
    const src = path.join(worktree, "src", "a.rs")
    const srcReal = fs.realpathSync(src)
    const dstStub = stubPermission([anchored(deniedDestinationReal)])
    const mv = await fsScopeDecision(
      dstStub.permission,
      ctx,
      [src, deniedDestination],
      [src, deniedDestination],
    )
    expect(mv.allowed).toBe(false)
    expect(dstStub.calls).toHaveLength(1)
    expect((dstStub.calls[0] as { resources: readonly string[] }).resources).toEqual([
      anchored(srcReal),
      anchored(deniedDestinationReal),
    ])
    expect(mv.reason).toContain(anchored(deniedDestinationReal))
    if (mv.allowed) fs.renameSync(src, deniedDestination)
    expect(existsSync(src)).toBe(true)
    expect(existsSync(deniedDestination)).toBe(false)

    // A path genuinely outside the main checkout has no main-relative spelling:
    // it stays ABSOLUTE (unchanged — not anchored) and is refused.
    const outsideStub = stubPermission([outsideReal])
    const external = await fsScopeDecision(
      outsideStub.permission,
      ctx,
      [outside],
      [outside],
    )
    expect(external.allowed).toBe(false)
    expect(outsideStub.calls).toHaveLength(1)
    expect((outsideStub.calls[0] as { resources: readonly string[] }).resources).toEqual([
      outsideReal,
    ])
    if (external.allowed) fs.unlinkSync(outside)
    expect(existsSync(outside)).toBe(true)
  } finally {
    fs.rmSync(path.dirname(repo), { recursive: true, force: true })
  }
}

test.skipIf(!enabled)(
  "e2e: worktree fs-scope asserts bare main-relative resources — round trip, denied destination, out-of-checkout",
  worktreeScopePermission,
)

// `mainCheckoutRoot`'s fs-scope frame is anchored on `context.worktree` ALONE
// when one was resolved, falling back to the plugin load location
// (`ctx.location.directory`) otherwise. Real I/O (fs + a git subprocess), so
// opt-in suite e2e.

test.skipIf(!enabled)(
  "e2e: mainCheckoutRoot's frame is anchored on context.worktree alone",
  async () => {
    const repo = scratchRepo()
    // macOS `os.tmpdir()` sits behind the `/var` → `/private/var` symlink, so
    // the resolver returns the canonical spelling, never the raw `mkdtemp`
    // string `scratchRepo` hands back.
    const repoReal = fs.realpathSync(repo)
    try {
      const nested = path.join(repo, "pkg")
      // mainCheckoutRoot with no resolved worktree: the frame comes from
      // ctx.location.directory (canonicalised).
      expect(await mainCheckoutRoot(v2Context(repo))).toEqual({ root: repoReal, project: null })
      expect(await mainCheckoutRoot(v2Context(nested))).toEqual({ root: repoReal, project: null })
      // A resolved worktree is the sole anchor: the location cannot override it.
      expect(await mainCheckoutRoot(v2Context(os.tmpdir(), { worktree: repoReal }))).toEqual({
        root: repoReal,
        project: null,
      })
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

// A caller with NO resolved project worktree (`context.worktree`) is refused by
// every project-bound helper with the shared MISSING_WORKTREE_ERROR signal —
// the helpers never walk up from the load location or cwd to find a database.
// The string-returning helpers (runCypher, runCli) return
// the refusal verbatim; the helpers that hand back a structured value
// (`branchAddedRequirementNames` returns a Set) throw it, and `runInProject`
// throws its own pre-existing missing-resolved-project refusal, which is a
// distinct message. Every path is a pure refusal (no spawn, no fs write), so
// the opt-in suite-e2e gate alone enables it.
test.skipIf(!enabled)(
  "e2e: a worktree-less context is refused by every project-bound helper",
  async () => {
    const context = v2Context(os.tmpdir())
    expect(context.worktree).toBeUndefined()

    expect(await runCypher(context, "MATCH (n) RETURN n")).toBe(MISSING_WORKTREE_ERROR)
    expect(await runCli(context, ["query", "MATCH (n) RETURN n"])).toBe(MISSING_WORKTREE_ERROR)

    let branchRefusal = ""
    try {
      await branchAddedRequirementNames(context)
    } catch (e) {
      branchRefusal = e instanceof Error ? e.message : String(e)
    }
    expect(branchRefusal).toBe(MISSING_WORKTREE_ERROR)

    let projectRefusal = ""
    try {
      await runInProject(context, ["project", "resolve", "main"])
    } catch (e) {
      projectRefusal = e instanceof Error ? e.message : String(e)
    }
    expect(projectRefusal).toContain("requires a resolved project")
    expect(projectRefusal).not.toBe(MISSING_WORKTREE_ERROR)
  },
)

// The ported suite tools are V2 `Plugin.define` plugins. Importing a tool
// module runs `Plugin.define({ id, setup })` at module scope; its `setup(ctx)`
// registers the tool through `ctx.tool.transform((editor) => editor.add({...}))`.
// Without a live opencode host this is pinned structurally: the plugin object
// carries the id + a setup function, a faked plugin context captures the added
// tool, and the tool's `execute` returns the `{ content }` result shape.
// `apg_query` requires a registered `project`; a project whose worktree has no
// database is refused by the binary, NAMING the project and its worktree, never
// the shared `NO_DB_ERROR` and never a fallback to another checkout's database.
test.skipIf(!enabled || !binary)(
  "e2e: a ported tool registers through Plugin.define/ctx.tool.transform(editor.add) and executes to {content}",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = bin

    // The default export IS the `Plugin.define` result (identity id + setup).
    expect(apgQuery.id).toBe("apg_query")
    expect(typeof apgQuery.setup).toBe("function")

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-nodb-home-"))
    fs.mkdirSync(path.join(home, ".config", "opencode", "node_modules", "@opencode", "plugin"), {
      recursive: true,
    })
    try {
      // `apg init` registers the built-in `main` row in the registry; no scan
      // follows, so the registered project's worktree has no database.
      const env = { ...process.env, HOME: home }
      const init = Bun.spawnSync({ cmd: [bin, "init", "."], cwd: repo, env, stdout: "pipe", stderr: "pipe" })
      expect(init.exitCode).toBe(0)

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
        location: { directory: repo },
        permission: { assert: async () => {} },
        tool: { transform: async (fn: (editorApi: typeof editor) => void) => void fn(editor) },
      }
      await apgQuery.setup(pluginCtx as never)

      expect(added).toHaveLength(1)
      const tool = added[0]
      expect(tool.name).toBe("apg_query")
      expect(typeof tool.description).toBe("string")
      expect(typeof tool.execute).toBe("function")

      // A required registered project: `main` resolves through the registry to
      // the main checkout, whose missing database is the binary's project-named
      // refusal — not the shared NO_DB_ERROR string.
      const result = await tool.execute!(
        { query: "MATCH (n) RETURN n", project: "main" },
        v2Context(repo),
      )
      const content = result.content
      expect(content.startsWith(QUERY_FAILED_PREFIX)).toBe(true)
      expect(content).toContain("refused: project `main`")
      expect(content).toContain(fs.realpathSync(repo))
      expect(content).not.toBe(NO_DB_ERROR)
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
)

// PER-INVOCATION durable writes (Model C). There is no session daemon: a
// durable `apg node`/`apg edge` mutation opens read-only, upgrades under the
// worktree's blocking lock for one critical section, writes its node file,
// makes exactly one commit and projects into db.lbug, then releases back to
// read-only — all in the ONE invocation, with no `apg session start|save|end`
// step anywhere. The removed `apg session` subcommand is not even dispatched,
// so every session lifecycle call exits non-zero and writes nothing. Real I/O
// (scratch /tmp git repo, a scanned db.lbug, git commits), so opt-in suite e2e
// against a candidate binary (`global.constraint.no-real-project-test`).
test.skipIf(!enabled || !binary)(
  "e2e: a durable apg node write lands its file, one commit and the projection in the invoking process, with no session lifecycle",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    const previousHome = process.env.HOME
    process.env.APG_BINARY = bin

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-invocation-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to `bun install`, and redirect HOME so nothing reads a real
    // `~/.config/opencode` (`global.constraint.no-real-project-test`).
    fs.mkdirSync(path.join(home, ".config", "opencode", "node_modules", "@opencode", "plugin"), {
      recursive: true,
    })
    process.env.HOME = home

    const env = { ...process.env, HOME: home }
    const runIn = (dir: string, args: string[], extraEnv: Record<string, string> = {}) =>
      Bun.spawnSync({
        cmd: [bin, ...args],
        cwd: dir,
        env: { ...env, ...extraEnv },
        stdout: "pipe",
        stderr: "pipe",
      })
    const run = (args: string[], extraEnv: Record<string, string> = {}) => runIn(repo, args, extraEnv)
    const git = (dir: string, args: string[]) =>
      Bun.spawnSync({ cmd: ["git", ...args], cwd: dir, env, stdout: "pipe", stderr: "pipe" })
    const commitCount = (dir: string) =>
      Number(git(dir, ["rev-list", "--count", "HEAD"]).stdout.toString().trim())

    try {
      expect(run(["init", "."]).exitCode).toBe(0)
      // Commit the scaffold (the documented scratch flow) so the durable
      // write's own commit lands on a clean tracked tree.
      git(repo, ["add", "-A"])
      git(repo, ["commit", "-q", "-m", "apg init"])

      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) throw new Error(`scan failed: ${scan.stderr.toString()}`)

      // Durable mutations are legal only inside a project worktree on the
      // project branch, so start a project from the scanned main checkout and
      // write from its worktree — the documented flow.
      const project = "suite-invocation"
      const startedProject = run(["project", "start", project], { APG_CALLER_SESSION: "s1" })
      if (startedProject.exitCode !== 0) {
        throw new Error(`project start failed: ${startedProject.stderr.toString()}`)
      }
      const worktree = path.join(repo, "apg", ".worktrees", project)
      const before = commitCount(worktree)

      // (a) The durable write completes in THIS invocation — no session is
      // started, and there is no save step to run afterwards.
      const nodeFile = path.join(
        worktree,
        "apg",
        "layers",
        "global",
        "constraint",
        "session-node.json",
      )
      const added = runIn(worktree, [
        "node",
        "add",
        "global",
        "constraint",
        "session-node",
        "--body",
        "durable per-invocation write",
      ])
      expect(added.exitCode).toBe(0)
      expect(added.stdout.toString().trim()).toBe("Added node global.constraint.session-node")

      // (b) Write-through: the node file exists the moment the invocation
      // returns, and the invocation made exactly one commit carrying it.
      expect(existsSync(nodeFile)).toBe(true)
      expect(commitCount(worktree)).toBe(before + 1)
      const committed = git(worktree, [
        "show",
        "--name-only",
        "--pretty=format:",
        "HEAD",
      ]).stdout.toString()
      expect(committed).toContain("apg/layers/global/constraint/session-node.json")

      // (c) The projection already landed: a plain read in a fresh invocation
      // sees the FQN, with no `apg session save` in between.
      const query = runIn(worktree, [
        "query",
        "MATCH (n:Constraint {fqn:'global.constraint.session-node'}) RETURN n.fqn",
      ])
      expect(query.exitCode).toBe(0)
      expect(query.stdout.toString()).toContain("global.constraint.session-node")

      // (d) No session lifecycle exists: the subcommand is not dispatched, so
      // each call exits non-zero naming the unknown subcommand, prints no
      // save/start success line, and writes nothing — no commit, clean tree,
      // the node file still in place.
      for (const sub of ["save", "start"]) {
        const session = runIn(worktree, ["session", sub])
        expect(session.exitCode).not.toBe(0)
        expect(session.stderr.toString()).toContain("apg: unknown subcommand: session")
        expect(session.stdout.toString()).not.toContain("Session saved")
        expect(session.stdout.toString()).not.toContain("Session started")
        expect(commitCount(worktree)).toBe(before + 1)
        expect(git(worktree, ["status", "--porcelain"]).stdout.toString().trim()).toBe("")
        expect(existsSync(nodeFile)).toBe(true)
      }
    } finally {
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

/** A scratch project served by an argv-recording stub binary: `project resolve`
 *  prints the resolved worktree (so the tool's project binding succeeds) and
 *  every other invocation records its argv, so `apg_scan`'s spawned scan can be
 *  inspected without a real scan — never against the apg repo
 *  (`global.constraint.no-real-project-test`). */
function scanStubTool(base: string): {
  repo: string
  worktree: string
  stub: string
  argv: () => string[]
} {
  const repo = path.join(base, "repo")
  const worktree = path.join(repo, "apg", ".worktrees", "suite-scan")
  fs.mkdirSync(path.join(repo, "apg", ".trans"), { recursive: true })
  fs.mkdirSync(worktree, { recursive: true })
  const recorded = path.join(base, "argv.txt")
  const stub = path.join(base, "stub-apg")
  fs.writeFileSync(
    stub,
    `#!/bin/sh\n` +
      `if [ "$1" = "project" ] && [ "$2" = "resolve" ]; then\n` +
      `  printf '%s\\n' '${worktree}'\n` +
      `  exit 0\n` +
      `fi\n` +
      `printf '%s\\n' "$@" > '${recorded}'\n` +
      `exit 0\n`,
    { mode: 0o755 },
  )
  const argv = () => fs.readFileSync(recorded, "utf8").split("\n").filter(Boolean)
  return { repo, worktree, stub, argv }
}

// The `apg_scan` tool's spawned argv carries `--force` when the caller asks for
// a from-scratch rebuild, and not otherwise. The required project resolves
// through the stub, and the scan runs in the RESOLVED WORKTREE — never an
// implicit workspace root.
test.skipIf(!enabled)(
  "e2e: apg_scan passes --force to the spawned scan only when force is set",
  async () => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-scan-force-"))
    const { repo, worktree, stub, argv } = scanStubTool(base)

    type AddedTool = {
      execute?: (input: unknown, context: ToolContext) => Promise<{ content: string }>
    }
    const added: AddedTool[] = []
    const editor = { add: (tool: AddedTool) => added.push(tool) }

    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = stub
    try {
      await apgScan.setup({
        location: { directory: repo },
        permission: { assert: async () => {} },
        tool: { transform: async (fn: (e: typeof editor) => void) => void fn(editor) },
      } as never)
      expect(added).toHaveLength(1)
      const tool = added[0]

      // force: true -> the spawned scan carries --force.
      expect(await tool.execute!({ project: "suite-scan", force: true }, v2Context(repo))).toEqual({
        content: "Scan completed successfully.",
      })
      expect(argv()).toEqual(["scan", worktree, "--force"])

      // force omitted -> the default scan argv, with no --force.
      expect(await tool.execute!({ project: "suite-scan" }, v2Context(repo))).toEqual({
        content: "Scan completed successfully.",
      })
      expect(argv()).toEqual(["scan", worktree])
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
  15000,
)

// The `apg_scan` tool forwards its `dialect` input as `--std <dialect>` (and
// nothing when omitted), mirroring the stub-argv pattern above.
test.skipIf(!enabled)(
  "e2e: apg_scan passes dialect to the spawned scan as --std only when set",
  async () => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-scan-std-"))
    const { repo, worktree, stub, argv } = scanStubTool(base)

    type AddedTool = {
      execute?: (input: unknown, context: ToolContext) => Promise<{ content: string }>
    }
    const added: AddedTool[] = []
    const editor = { add: (tool: AddedTool) => added.push(tool) }

    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = stub
    try {
      await apgScan.setup({
        location: { directory: repo },
        permission: { assert: async () => {} },
        tool: { transform: async (fn: (e: typeof editor) => void) => void fn(editor) },
      } as never)
      expect(added).toHaveLength(1)
      const tool = added[0]

      expect(
        await tool.execute!(
          { project: "suite-scan", language: "cobol", dialect: "cobol2002" },
          v2Context(repo),
        ),
      ).toEqual({
        content: "Scan completed successfully.",
      })
      expect(argv()).toEqual(["scan", worktree, "--language", "cobol", "--std", "cobol2002"])

      expect(await tool.execute!({ project: "suite-scan" }, v2Context(repo))).toEqual({
        content: "Scan completed successfully.",
      })
      expect(argv()).toEqual(["scan", worktree])
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
  15000,
)
