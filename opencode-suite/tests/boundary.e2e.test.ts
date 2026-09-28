// e2e boundary test for the curated suite tools: the stored<->absolute identity
// conversion (against a candidate `apg` binary) and the git branch-delta
// requirement wrapper (git/fs only). Both are real I/O, so both are opt-in
// suite e2e.
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
  canonicalPath,
  agentFsGlobs,
  mainCheckoutRoot,
  REQUIREMENT_FQN_PREFIX,
  csvToRows,
  type ToolContext,
} from "../lib/apg.ts"

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

      const context: ToolContext = { directory: repo, worktree: repo }
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
      const context: ToolContext = { directory: worktree, worktree }
      const added = await branchAddedRequirementNames(context, worktree)

      // Exactly the requirement node file added on this branch — not the
      // pre-existing one already carried by the default branch.
      expect([...added].sort()).toEqual(["req-branch-added"])
      expect(added.has("req-preexisting")).toBe(false)

      // The main checkout, on the default branch, sees an empty delta: no false
      // positives.
      const none = await branchAddedRequirementNames(
        { directory: repo, worktree: repo },
        repo,
      )
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
      const context: ToolContext = { directory: worktree, worktree }
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
      const context: ToolContext = { directory: worktree, worktree }
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

// agent-fs-tools phase-01.task-8: the PLUGIN-FREE self-enforcement core. These
// scenarios import ONLY `../lib/apg.ts` (no `@opencode-ai/plugin`, so no
// node_modules/network is needed) and pass the acting agent's granted globs as
// a LITERAL argument — the context shape the task-1 spike pins is irrelevant
// here. Each scenario evaluates `fsScopeDecision` AND applies the
// corresponding real fs op gated on the decision, against a scratch /tmp git
// repo (`global.constraint.no-real-project-test`).

/// A scratch git repo with an in-grant `src/` tree and an out-of-grant
/// `outside.txt`, committed. Returns its root.
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

test.skipIf(!enabled)(
  "e2e: fsScopeDecision gates a real unlink — in-grant removed, out-of-grant refused",
  () => {
    const repo = scratchFsRepo()
    try {
      const grants = ["src/*.rs"]

      // In-grant: allowed, so the real op mutates the tree.
      const inGrant = path.join(repo, "src", "a.rs")
      const allowed = fsScopeDecision(repo, inGrant, grants)
      expect(allowed.allowed).toBe(true)
      if (allowed.allowed) fs.unlinkSync(inGrant)
      expect(existsSync(inGrant)).toBe(false)

      // Out-of-grant: refused, so the tree is untouched.
      const outGrant = path.join(repo, "outside.txt")
      const refused = fsScopeDecision(repo, outGrant, grants)
      expect(refused.allowed).toBe(false)
      if (refused.allowed) fs.unlinkSync(outGrant)
      expect(existsSync(outGrant)).toBe(true)

      // Boundary escape (`..`) is refused even when a glob would match.
      const escape = path.join(repo, "..", "escape.txt")
      const d = fsScopeDecision(repo, escape, ["**/*"])
      expect(d.allowed).toBe(false)
      expect(d.inBoundary).toBe(false)
      expect(d.reason).toContain("boundary")
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: fsScopeDecision gates a real rename — BOTH endpoints must be in grant",
  () => {
    const repo = scratchFsRepo()
    try {
      const grants = ["src/*.rs"]

      // Both endpoints in grant: the rename mutates the tree.
      const from = path.join(repo, "src", "a.rs")
      const to = path.join(repo, "src", "renamed.rs")
      expect(fsScopeDecision(repo, from, grants).allowed).toBe(true)
      expect(fsScopeDecision(repo, to, grants).allowed).toBe(true)
      fs.renameSync(from, to)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(from)).toBe(false)

      // Destination out of grant: refused, tree untouched.
      const badTo = path.join(repo, "outside-moved.rs")
      const dFrom = fsScopeDecision(repo, to, grants)
      const dTo = fsScopeDecision(repo, badTo, grants)
      expect(dFrom.allowed).toBe(true)
      expect(dTo.allowed).toBe(false)
      if (dFrom.allowed && dTo.allowed) fs.renameSync(to, badTo)
      expect(existsSync(to)).toBe(true)
      expect(existsSync(badTo)).toBe(false)

      // Source out of grant: also refused.
      const badFrom = path.join(repo, "outside.txt")
      expect(fsScopeDecision(repo, badFrom, grants).allowed).toBe(false)
      expect(existsSync(badFrom)).toBe(true)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: fsScopeDecision gates a real copy — BOTH endpoints must be in grant",
  () => {
    const repo = scratchFsRepo()
    try {
      const grants = ["src/*.rs"]

      // Both endpoints in grant: the copy mutates the tree (source stays).
      const from = path.join(repo, "src", "a.rs")
      const to = path.join(repo, "src", "copy.rs")
      expect(fsScopeDecision(repo, from, grants).allowed).toBe(true)
      expect(fsScopeDecision(repo, to, grants).allowed).toBe(true)
      fs.cpSync(from, to, { recursive: true })
      expect(existsSync(to)).toBe(true)
      expect(existsSync(from)).toBe(true)

      // Destination out of grant: refused, tree untouched.
      const badTo = path.join(repo, "outside-copy.rs")
      const dFrom = fsScopeDecision(repo, from, grants)
      const dTo = fsScopeDecision(repo, badTo, grants)
      expect(dFrom.allowed).toBe(true)
      expect(dTo.allowed).toBe(false)
      if (dFrom.allowed && dTo.allowed) fs.cpSync(from, badTo, { recursive: true })
      expect(existsSync(badTo)).toBe(false)

      // Source out of grant: also refused.
      const badFrom = path.join(repo, "outside.txt")
      expect(fsScopeDecision(repo, badFrom, grants).allowed).toBe(false)
      expect(existsSync(path.join(repo, "src", "copy.rs"))).toBe(true)
    } finally {
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
    }
  },
)

// agent-fs-tools feedback-8: `agentFsGlobs` — the acting-agent permission read
// that feeds `fsScopeDecision` — is real file I/O, so it is covered here at the
// opt-in e2e tier, plugin-free (imports only `../lib/apg.ts` + node/bun
// builtins). `HOME` is redirected to the scratch tree so the global fallback
// never reads a real `$HOME` (`global.constraint.no-real-project-test`).

/// A scratch base with a project dir (holding one agent file) and an isolated
/// HOME. Returns the base (for cleanup), the project root, and the scratch home.
function scratchAgentDir(
  name: string,
  frontmatter: string,
): { base: string; repo: string; home: string } {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-agentfs-"))
  const repo = path.join(base, "repo")
  const home = path.join(base, "home")
  fs.mkdirSync(path.join(repo, ".opencode", "agents"), { recursive: true })
  fs.mkdirSync(home, { recursive: true })
  fs.writeFileSync(path.join(repo, ".opencode", "agents", `${name}.md`), frontmatter)
  return { base, repo, home }
}

test.skipIf(!enabled)(
  "e2e: agentFsGlobs reads the project agent's permission.edit rules in file order (allow AND deny)",
  () => {
    const agent = "scoped-agent"
    const file = [
      "---",
      "description: scratch scoped agent",
      "permission:",
      '  "*": deny',
      "  edit:",
      '    "*": deny',
      '    "src/*.rs": allow',
      '    "opencode-suite/**": allow',
      '    "secret/**": deny',
      "---",
      "",
      "# Scoped agent",
      "",
    ].join("\n")
    const { base, repo, home } = scratchAgentDir(agent, file)
    const prevHome = process.env.HOME
    process.env.HOME = home
    try {
      const rules = agentFsGlobs({ agent, directory: repo, worktree: repo }, repo)
      // BOTH allow AND deny entries survive, in FILE ORDER — the ordered
      // `{glob, allow}` rules the last-match decision consumes. A parser that
      // dropped the deny entries (the old allow-only projection) fails here.
      expect(rules).toEqual([
        { glob: "*", allow: false },
        { glob: "src/*.rs", allow: true },
        { glob: "opencode-suite/**", allow: true },
        { glob: "secret/**", allow: false },
      ])
      expect(rules.map((r) => r.glob)).not.toContain("build/**")

      // The read result feeds scope enforcement with LAST-MATCH-WINS semantics:
      // an allow glob passes, an explicit deny revokes, and a path matched by no
      // rule is refused (deny-by-default).
      expect(fsScopeDecision(repo, path.join(repo, "src", "a.rs"), rules).allowed).toBe(true)
      expect(fsScopeDecision(repo, path.join(repo, "opencode-suite", "x.ts"), rules).allowed).toBe(
        true,
      )
      // `secret/**` is an EXPLICIT deny, so the path is refused BY THE DENY rule
      // (a rule matched, and it was a deny) — not merely un-granted.
      const secret = fsScopeDecision(repo, path.join(repo, "secret", "k.txt"), rules)
      expect(secret.allowed).toBe(false)
      expect(secret.globMatched).toBe(true)
      // No rule matches `build/x.rs` at all: refused by default.
      const unmatched = fsScopeDecision(repo, path.join(repo, "build", "x.rs"), rules)
      expect(unmatched.allowed).toBe(false)
      expect(unmatched.globMatched).toBe(false)
    } finally {
      if (prevHome === undefined) delete process.env.HOME
      else process.env.HOME = prevHome
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: agentFsGlobs fails closed when no agent file resolves, and reads the global fallback",
  () => {
    // The project holds an unrelated agent, and the isolated HOME is empty.
    const { base, repo, home } = scratchAgentDir(
      "present-agent",
      '---\npermission:\n  edit:\n    "src/*.rs": allow\n---\n',
    )
    const prevHome = process.env.HOME
    process.env.HOME = home
    try {
      // Missing project file + empty HOME: fail-closed `[]`, never a default.
      expect(agentFsGlobs({ agent: "unknown-agent", directory: repo, worktree: repo }, repo)).toEqual(
        [],
      )
      // No agent name at all is likewise fail-closed.
      expect(agentFsGlobs({ directory: repo, worktree: repo }, repo)).toEqual([])

      // With no project file, the globally-installed agent file is the fallback
      // (`apg init` installs the distributed suite under `~/.opencode/agents/`).
      fs.mkdirSync(path.join(home, ".opencode", "agents"), { recursive: true })
      fs.writeFileSync(
        path.join(home, ".opencode", "agents", "global-agent.md"),
        '---\npermission:\n  edit:\n    "lib/*.ts": allow\n---\n',
      )
      expect(
        agentFsGlobs({ agent: "global-agent", directory: repo, worktree: repo }, repo),
      ).toEqual([{ glob: "lib/*.ts", allow: true }])
    } finally {
      if (prevHome === undefined) delete process.env.HOME
      else process.env.HOME = prevHome
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

// worktree-write-scope phase-01.task-8: the fs tools' MAIN-ANCHORED scope gate
// against a REAL scratch git repo in the documented `<main>/apg/.worktrees/<p>`
// linked-worktree layout. The tool modules import `@opencode-ai/plugin` (which
// this plugin-free suite does not install), so — like the `fsScopeDecision`
// scenarios above — each scenario drives the SAME plugin-free sequence the tool
// bodies run (`mainCheckoutRoot` → `agentFsGlobs` → `canonicalPath` →
// `fsScopeDecision`) and applies the corresponding real fs op gated on the
// decision. Pure fs/git, no
// candidate `apg` binary, so these gate on the e2e opt-in alone. The whole repo
// lives under the OS temp dir and is removed in `finally`
// (`global.constraint.no-real-project-test`).

const WORKTREE_AGENT = "worktree-impl"

/// A scratch git repo laid out as an apg project: the MAIN checkout carries the
/// acting agent's file — grants written against the main root as
/// `apg/.worktrees/*/<glob>` (plus a root `src/*.rs` grant the structural rule
/// must override) — and a `src/` tree; a LINKED worktree sits at the documented
/// `<main>/apg/.worktrees/<project>` path. The base is realpath'd so
/// `mainCheckoutRoot`'s canonical main root and the candidate paths share one
/// frame (macOS `os.tmpdir()` sits behind a symlink).
function scratchWorktreeRepo(): { base: string; main: string; worktree: string; project: string } {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-wtscope-")))
  const main = path.join(base, "repo")
  const project = "proj"
  fs.mkdirSync(path.join(main, "src"), { recursive: true })
  fs.mkdirSync(path.join(main, ".opencode", "agents"), { recursive: true })
  fs.writeFileSync(path.join(main, "src", "a.rs"), "fn a() {}\n")
  fs.writeFileSync(path.join(main, "src", "b.rs"), "fn b() {}\n")
  fs.writeFileSync(
    path.join(main, ".opencode", "agents", `${WORKTREE_AGENT}.md`),
    [
      "---",
      "description: scratch worktree code-writer",
      "permission:",
      '  "*": deny',
      "  edit:",
      '    "*": deny',
      // A root grant that matches the main checkout's own `src/*.rs`; the
      // structural worktree rule refuses that path regardless.
      '    "src/*.rs": allow',
      // The real code-writer grant: worktree-rooted, main-root-relative.
      '    "apg/.worktrees/*/src/*.rs": allow',
      "---",
      "",
    ].join("\n"),
  )
  const run = (cmd: string[], cwd: string = main) =>
    Bun.spawnSync({ cmd, cwd, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", "main"])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])
  // The project worktree at the documented path — the real apg topology.
  const worktree = path.join(main, "apg", ".worktrees", project)
  const add = run(["git", "worktree", "add", "-q", "-b", "feature", worktree])
  if (add.exitCode !== 0) throw new Error(`git worktree add failed: ${add.stderr.toString()}`)
  return { base, main, worktree, project }
}

/// The plugin-free gate the fs tool bodies run: resolve the MAIN checkout frame
/// from the caller's `directory`, read the acting agent's main-root-relative
/// grants from that frame, then for each candidate decide the SCOPE on BOTH the
/// caller-NAMED entry (`entry` — `canonicalPath(dirname(named))` + the basename
/// as-is: the link's OWN location, what the op actually changes) AND the fully
/// canonicalised path (`canonicalPath`, following the final symlink), while
/// carrying the caller-NAMED path (`named`, final segment unresolved) the body
/// would actually act on. `allowed` requires BOTH; `reason` is the first
/// refusal (entry first, then resolved), so a main-located link into the
/// worktree is refused on its entry. Absolute paths win; relative paths resolve
/// against `directory`. Returns the resolved frame and one entry/decision set
/// per candidate.
async function fsGate(
  directory: string,
  candidates: string[],
  agent: string = WORKTREE_AGENT,
): Promise<{
  root: string
  project: string | null
  decisions: {
    named: string
    entry: string
    abs: string
    entryAllowed: boolean
    entryReason: string | null
    allowed: boolean
    reason: string | null
    globMatched: boolean
    inBoundary: boolean
  }[]
}> {
  const context: ToolContext = { agent, directory, worktree: directory }
  const frame = await mainCheckoutRoot(context, directory)
  expect(frame).not.toBeNull()
  const { root, project } = frame as { root: string; project: string | null }
  const granted = agentFsGlobs(context, root, root)
  const dir = canonicalPath(directory)
  const decisions = candidates.map((c) => {
    const named = path.resolve(dir, c)
    const entry = path.join(canonicalPath(path.dirname(named)), path.basename(named))
    const abs = canonicalPath(named)
    const entryD = fsScopeDecision(root, entry, granted, project)
    const resolvedD = fsScopeDecision(root, abs, granted, project)
    return {
      named,
      entry,
      abs,
      entryAllowed: entryD.allowed,
      entryReason: entryD.reason,
      allowed: entryD.allowed && resolvedD.allowed,
      reason: entryD.allowed ? resolvedD.reason : entryD.reason,
      globMatched: resolvedD.globMatched,
      inBoundary: resolvedD.inBoundary,
    }
  })
  return { root, project, decisions }
}

test.skipIf(!enabled)(
  "e2e: apg_rm's main-root frame refuses a main-checkout path whatever directory, and removes under an owned worktree grant",
  async () => {
    const { base, main, worktree, project } = scratchWorktreeRepo()
    try {
      const mainFile = path.join(main, "src", "a.rs")
      // directory=main AND directory=worktree: a main-checkout path is refused
      // structurally — the root `src/*.rs` grant matches, yet it stays denied —
      // and the gated unlink never runs.
      for (const directory of [main, worktree]) {
        const gate = await fsGate(directory, [mainFile])
        expect(gate.root).toBe(main)
        expect(gate.project).toBe(directory === worktree ? project : null)
        const d = gate.decisions[0]
        expect(d.allowed).toBe(false)
        expect(d.inBoundary).toBe(true)
        expect(d.globMatched).toBe(true)
        expect(d.reason).toBe("path resolves into the main checkout")
        if (d.allowed) fs.rmSync(d.abs)
        expect(existsSync(mainFile)).toBe(true)
      }

      // A `..` escape is refused (boundary), even under the owned worktree
      // globs.
      const escaped = await fsGate(worktree, [`${worktree}/../../../../escape.txt`])
      expect(escaped.decisions[0].allowed).toBe(false)
      expect(escaped.decisions[0].inBoundary).toBe(false)
      expect(escaped.decisions[0].reason).toContain("boundary")
      if (escaped.decisions[0].allowed) fs.rmSync(escaped.decisions[0].abs)

      // Under the owned worktree grant the rm is allowed (real unlink).
      const wtFile = path.join(worktree, "src", "b.rs")
      const ok = await fsGate(worktree, [wtFile])
      expect(ok.decisions[0].allowed).toBe(true)
      expect(ok.decisions[0].reason).toBe(null)
      if (ok.decisions[0].allowed) fs.rmSync(ok.decisions[0].abs)
      expect(existsSync(wtFile)).toBe(false)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: apg_mv's main-root frame refuses either endpoint in main whatever directory, and moves both under an owned worktree grant",
  async () => {
    const { base, main, worktree, project } = scratchWorktreeRepo()
    try {
      const mainFile = path.join(main, "src", "a.rs")
      const wtFile = path.join(worktree, "src", "b.rs")
      for (const directory of [main, worktree]) {
        // Source in the main checkout: refused.
        const srcMain = await fsGate(directory, [mainFile, path.join(worktree, "src", "a-moved.rs")])
        expect(srcMain.root).toBe(main)
        expect(srcMain.project).toBe(directory === worktree ? project : null)
        expect(srcMain.decisions[0].allowed).toBe(false)
        expect(srcMain.decisions[0].reason).toBe("path resolves into the main checkout")

        // Destination in the main checkout: refused too — BOTH endpoints are
        // checked, so a permissive source cannot smuggle a write into main.
        const dstMain = await fsGate(directory, [wtFile, mainFile])
        expect(dstMain.decisions[0].allowed).toBe(true)
        expect(dstMain.decisions[1].allowed).toBe(false)
        expect(dstMain.decisions[1].reason).toBe("path resolves into the main checkout")
        if (dstMain.decisions[0].allowed && dstMain.decisions[1].allowed) {
          fs.renameSync(dstMain.decisions[0].abs, dstMain.decisions[1].abs)
        }
        expect(existsSync(wtFile)).toBe(true)
        expect(existsSync(mainFile)).toBe(true)
      }

      // A `..` destination escape is refused (boundary).
      const escaped = await fsGate(worktree, [wtFile, `${worktree}/../../../../escape.rs`])
      expect(escaped.decisions[1].allowed).toBe(false)
      expect(escaped.decisions[1].inBoundary).toBe(false)
      expect(escaped.decisions[1].reason).toContain("boundary")

      // Both endpoints under the owned worktree grant: the move happens.
      const to = path.join(worktree, "src", "b-moved.rs")
      const ok = await fsGate(worktree, [wtFile, to])
      expect(ok.decisions[0].allowed).toBe(true)
      expect(ok.decisions[1].allowed).toBe(true)
      if (ok.decisions[0].allowed && ok.decisions[1].allowed) {
        fs.renameSync(ok.decisions[0].abs, ok.decisions[1].abs)
      }
      expect(existsSync(to)).toBe(true)
      expect(existsSync(wtFile)).toBe(false)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: apg_cp's main-root frame refuses a main-checkout destination whatever directory, and copies under an owned worktree grant",
  async () => {
    const { base, main, worktree, project } = scratchWorktreeRepo()
    try {
      const wtSource = path.join(worktree, "src", "a.rs")
      const mainDest = path.join(main, "src", "a-copied.rs")
      for (const directory of [main, worktree]) {
        // A copy WRITES only at the destination, so only that is decided: a
        // source in the worktree cannot copy INTO the main checkout.
        const gate = await fsGate(directory, [wtSource, mainDest])
        expect(gate.root).toBe(main)
        expect(gate.project).toBe(directory === worktree ? project : null)
        expect(gate.decisions[1].allowed).toBe(false)
        expect(gate.decisions[1].inBoundary).toBe(true)
        expect(gate.decisions[1].reason).toBe("path resolves into the main checkout")
        if (gate.decisions[1].allowed) {
          fs.cpSync(gate.decisions[0].abs, gate.decisions[1].abs, { recursive: true })
        }
        expect(existsSync(mainDest)).toBe(false)
        expect(existsSync(wtSource)).toBe(true)
      }

      // A `..` destination escape is refused (boundary).
      const escaped = await fsGate(worktree, [wtSource, `${worktree}/../../../../escape.rs`])
      expect(escaped.decisions[1].allowed).toBe(false)
      expect(escaped.decisions[1].inBoundary).toBe(false)
      expect(escaped.decisions[1].reason).toContain("boundary")

      // Destination under the owned worktree grant: the copy happens.
      const wtDest = path.join(worktree, "src", "a-copy.rs")
      const ok = await fsGate(worktree, [wtSource, wtDest])
      expect(ok.decisions[1].allowed).toBe(true)
      expect(ok.decisions[1].reason).toBe(null)
      if (ok.decisions[1].allowed) {
        fs.cpSync(ok.decisions[0].abs, ok.decisions[1].abs, { recursive: true })
      }
      expect(existsSync(wtDest)).toBe(true)
      expect(existsSync(wtSource)).toBe(true)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: mainCheckoutRoot resolves the main checkout root and project name from inside the worktree",
  async () => {
    const { base, main, worktree, project } = scratchWorktreeRepo()
    try {
      // From the worktree root and a nested dir, the documented
      // `<main>/apg/.worktrees/<project>` layout decodes to the MAIN checkout.
      for (const dir of [worktree, path.join(worktree, "src")]) {
        const frame = await mainCheckoutRoot(
          { agent: WORKTREE_AGENT, directory: dir, worktree: dir },
          dir,
        )
        expect(frame).toEqual({ root: main, project })
      }
      // From the main checkout the root is the same and there is no project.
      const fromMain = await mainCheckoutRoot(
        { agent: WORKTREE_AGENT, directory: main, worktree: main },
        main,
      )
      expect(fromMain).toEqual({ root: main, project: null })
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

/// The same scratch repo as `scratchWorktreeRepo`, but the returned `main` and
/// `worktree` roots are spelled THROUGH A SYMLINK (`<base>/alias -> <base>`)
/// that has NOT been realpath'd — the macOS `/tmp`-style frame where
/// `mainCheckoutRoot` resolves the root symlink-free but a candidate spelled
/// from the caller's raw `directory` would not, unless the candidate is
/// canonicalised too. `realMain` is the resolved root the tool must agree on;
/// `base` (canonical) is for cleanup.
function scratchSymlinkedWorktreeRepo(): {
  base: string
  realMain: string
  main: string
  worktree: string
  project: string
} {
  const { base, main, project } = scratchWorktreeRepo()
  const alias = path.join(base, "alias")
  fs.symlinkSync(base, alias)
  return {
    base,
    realMain: main,
    main: path.join(alias, "repo"),
    worktree: path.join(alias, "repo", "apg", ".worktrees", project),
    project,
  }
}

test.skipIf(!enabled)(
  "e2e: a symlinked directory frame still allows a legitimate worktree write",
  async () => {
    const { base, realMain, worktree, project } = scratchSymlinkedWorktreeRepo()
    try {
      // The caller's frame passes THROUGH the symlink (`directory` is not
      // realpath'd); `mainCheckoutRoot` resolves the REAL main root, and
      // canonicalising the candidate puts it in that same frame — so the owned
      // worktree write is allowed, not misread as a `..` boundary escape.
      const wtFile = path.join(worktree, "src", "b.rs")
      const gate = await fsGate(worktree, [wtFile])
      expect(gate.root).toBe(realMain)
      expect(gate.project).toBe(project)
      const canonical = path.join(realMain, "apg", ".worktrees", project, "src", "b.rs")
      expect(gate.decisions[0].abs).toBe(canonical)
      expect(gate.decisions[0].allowed).toBe(true)
      expect(gate.decisions[0].reason).toBe(null)
      if (gate.decisions[0].allowed) fs.rmSync(gate.decisions[0].abs)
      expect(existsSync(canonical)).toBe(false)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

test.skipIf(!enabled)(
  "e2e: an mv/cp destination through a worktree symlink into main is refused",
  async () => {
    const { base, main, worktree } = scratchWorktreeRepo()
    try {
      // A symlink INSIDE the worktree points at a main-checkout file, so the
      // destination is LEXICALLY under the owned worktree glob
      // (`apg/.worktrees/proj/src/*.rs`) while its real target is in main. A
      // text-only check would allow it; canonicalising the destination follows
      // the symlink into main and refuses it.
      const mainFile = path.join(main, "src", "a.rs")
      const mainBefore = fs.readFileSync(mainFile, "utf8")
      const wtSource = path.join(worktree, "src", "b.rs")
      const trap = path.join(worktree, "src", "trap.rs")
      fs.symlinkSync(mainFile, trap)

      // mv: the worktree source is allowed, the symlinked destination is refused
      // because it canonicalises into the main checkout.
      const mv = await fsGate(worktree, [wtSource, trap])
      expect(mv.decisions[0].allowed).toBe(true)
      expect(mv.decisions[1].allowed).toBe(false)
      expect(mv.decisions[1].inBoundary).toBe(true)
      expect(mv.decisions[1].globMatched).toBe(true)
      expect(mv.decisions[1].reason).toBe("path resolves into the main checkout")
      if (mv.decisions[0].allowed && mv.decisions[1].allowed) {
        fs.renameSync(mv.decisions[0].abs, mv.decisions[1].abs)
      }
      // The gated op never ran: the symlink is intact and main's file is
      // unchanged (a real rename would have replaced main's `a.rs`).
      expect(fs.lstatSync(trap).isSymbolicLink()).toBe(true)
      expect(fs.readFileSync(mainFile, "utf8")).toBe(mainBefore)
      expect(existsSync(wtSource)).toBe(true)

      // cp: the same destination is refused on the destination check.
      const cp = await fsGate(worktree, [wtSource, trap])
      expect(cp.decisions[1].allowed).toBe(false)
      expect(cp.decisions[1].reason).toBe("path resolves into the main checkout")
      if (cp.decisions[1].allowed) {
        fs.cpSync(cp.decisions[0].abs, cp.decisions[1].abs, { recursive: true })
      }
      expect(fs.readFileSync(mainFile, "utf8")).toBe(mainBefore)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

// worktree-write-scope feedback-10: `canonicalPath` resolves the FINAL segment
// too, and the buggy tools acted on that resolved path — so `rm`/`mv` of an
// in-worktree symlink changed the file it POINTED AT and left the link behind.
// The fix keeps the SCOPE decision on the fully resolved path (a link into main
// is still refused — see the test above) while the destructive op targets the
// caller-NAMED path, so POSIX symlink semantics hold: rm unlinks the link, mv
// renames the link, and the target file is untouched.
test.skipIf(!enabled)(
  "e2e: rm/mv of an in-worktree symlink act on the NAMED link, not its target",
  async () => {
    const { base, worktree } = scratchWorktreeRepo()
    try {
      // `link -> target`, BOTH inside the owned worktree `src/*.rs` grant. The
      // scope decision resolves the link to `target` (in-grant, allowed); the op
      // target is the link itself, so the target file must survive untouched.
      const target = path.join(worktree, "src", "a.rs")
      const targetBefore = fs.readFileSync(target, "utf8")

      // rm: removes ONLY the link.
      const rmLink = path.join(worktree, "src", "rm-link.rs")
      fs.symlinkSync(target, rmLink)
      const rmGate = await fsGate(worktree, [rmLink])
      expect(rmGate.decisions[0].allowed).toBe(true)
      expect(rmGate.decisions[0].abs).toBe(target)
      expect(rmGate.decisions[0].named).toBe(rmLink)
      if (rmGate.decisions[0].allowed) fs.rmSync(rmGate.decisions[0].named)
      expect(existsSync(rmLink)).toBe(false)
      expect(existsSync(target)).toBe(true)
      expect(fs.readFileSync(target, "utf8")).toBe(targetBefore)

      // mv: renames ONLY the link (the renamed name is still a symlink to the
      // same target); the target file is untouched.
      const mvLink = path.join(worktree, "src", "mv-link.rs")
      const mvTo = path.join(worktree, "src", "mv-renamed.rs")
      fs.symlinkSync(target, mvLink)
      const mvGate = await fsGate(worktree, [mvLink, mvTo])
      expect(mvGate.decisions[0].allowed).toBe(true)
      expect(mvGate.decisions[0].abs).toBe(target)
      expect(mvGate.decisions[0].named).toBe(mvLink)
      expect(mvGate.decisions[1].allowed).toBe(true)
      if (mvGate.decisions[0].allowed && mvGate.decisions[1].allowed) {
        fs.renameSync(mvGate.decisions[0].named, mvGate.decisions[1].named)
      }
      expect(existsSync(mvLink)).toBe(false)
      expect(fs.lstatSync(mvTo).isSymbolicLink()).toBe(true)
      expect(fs.readlinkSync(mvTo)).toBe(target)
      expect(existsSync(target)).toBe(true)
      expect(fs.readFileSync(target, "utf8")).toBe(targetBefore)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

// worktree-write-scope phase-04.task-11: the agents' `permission.edit` scope is
// an ORDERED rule list — an ALLOW entry grants, a DENY revokes, and the LAST
// matching entry wins, with a path matched by no entry refused by default. A
// deny-anywhere-wins (or allow-only) implementation gets the first case below
// wrong, and a deny-anywhere-wins implementation additionally gets the
// re-granted file wrong. This exercises the same plugin-free sequence the
// `apg_rm`/`apg_mv`/`apg_cp` bodies run (`mainCheckoutRoot` → `agentFsGlobs` →
// `canonicalPath` → `fsScopeDecision`) and applies the corresponding real fs op
// gated on the decision, against a scratch /tmp git repo
// (`global.constraint.no-real-project-test`).

/// The acting agent whose edit rules carry a broad worktree ALLOW, a LATER deny
/// over a subtree, and a still-later re-grant for one file.
const DENY_AGENT = "worktree-deny-impl"

/// A scratch project repo laid out exactly like `scratchWorktreeRepo`, plus an
/// agent (`DENY_AGENT`) whose edit rules are ordered
/// `allow(apg/.worktrees/*/src/**)` → `deny(.../src/secret/**)` →
/// `allow(.../src/secret/ok.rs)`, and a `src/secret/` tree under the worktree:
/// `secret/k.rs` is revoked by the later deny, `secret/ok.rs` is re-granted by
/// the even-later allow.
function scratchDenyWorktreeRepo(): {
  base: string
  main: string
  worktree: string
  project: string
} {
  const repo = scratchWorktreeRepo()
  fs.writeFileSync(
    path.join(repo.main, ".opencode", "agents", `${DENY_AGENT}.md`),
    [
      "---",
      "description: scratch worktree code-writer with a later deny",
      "permission:",
      '  "*": deny',
      "  edit:",
      // A broad ALLOW over the whole owned worktree...
      '    "apg/.worktrees/*/src/**": allow',
      // ...REVOKED for this subtree by a LATER deny (the last match wins)...
      '    "apg/.worktrees/*/src/secret/**": deny',
      // ...and RE-GRANTED for one file by an even later allow.
      '    "apg/.worktrees/*/src/secret/ok.rs": allow',
      "---",
      "",
    ].join("\n"),
  )
  fs.mkdirSync(path.join(repo.worktree, "src", "secret"), { recursive: true })
  fs.writeFileSync(path.join(repo.worktree, "src", "secret", "k.rs"), "fn k() {}\n")
  fs.writeFileSync(path.join(repo.worktree, "src", "secret", "ok.rs"), "fn ok() {}\n")
  return repo
}

test.skipIf(!enabled)(
  "e2e: a later deny revokes an earlier broad allow for rm/mv/cp, a later allow re-grants",
  async () => {
    const { base, main, worktree, project } = scratchDenyWorktreeRepo()
    try {
      const allowedFile = path.join(worktree, "src", "a.rs")
      const deniedDir = path.join(worktree, "src", "secret")
      const deniedFile = path.join(deniedDir, "k.rs")
      const regrantedFile = path.join(deniedDir, "ok.rs")

      // The broad ALLOW matches all three; ONLY the ordered last-match decision
      // distinguishes them: the later DENY revokes `secret/**`, the still-later
      // ALLOW re-grants `secret/ok.rs`.
      const gate = await fsGate(worktree, [allowedFile, deniedFile, regrantedFile], DENY_AGENT)
      expect(gate.root).toBe(main)
      expect(gate.project).toBe(project)
      expect(gate.decisions[0].allowed).toBe(true)
      expect(gate.decisions[1].allowed).toBe(false)
      // A rule DID match the denied path and it was a deny (not "no match") —
      // the earlier broad allow was revoked by the later deny.
      expect(gate.decisions[1].globMatched).toBe(true)
      expect(gate.decisions[1].reason).toBe("path is not within the acting agent's granted globs")
      // A later ALLOW after the deny re-grants, so deny-anywhere-wins fails here.
      expect(gate.decisions[2].allowed).toBe(true)

      // rm: the denied file's real op is skipped — the tree is untouched.
      if (gate.decisions[1].allowed) fs.rmSync(gate.decisions[1].named)
      expect(existsSync(deniedFile)).toBe(true)

      // mv: an in-grant source into the denied subtree is refused on the
      // DESTINATION, so the move never runs.
      const mvDest = path.join(deniedDir, "moved.rs")
      const mvGate = await fsGate(worktree, [allowedFile, mvDest], DENY_AGENT)
      expect(mvGate.decisions[0].allowed).toBe(true)
      expect(mvGate.decisions[1].allowed).toBe(false)
      expect(mvGate.decisions[1].globMatched).toBe(true)
      if (mvGate.decisions[0].allowed && mvGate.decisions[1].allowed) {
        fs.renameSync(mvGate.decisions[0].named, mvGate.decisions[1].named)
      }
      expect(existsSync(allowedFile)).toBe(true)
      expect(existsSync(mvDest)).toBe(false)

      // cp: the same denied destination is refused, so nothing is written.
      const cpGate = await fsGate(worktree, [allowedFile, mvDest], DENY_AGENT)
      expect(cpGate.decisions[1].allowed).toBe(false)
      expect(cpGate.decisions[1].globMatched).toBe(true)
      if (cpGate.decisions[1].allowed) {
        fs.cpSync(cpGate.decisions[0].named, cpGate.decisions[1].named, { recursive: true })
      }
      expect(existsSync(mvDest)).toBe(false)

      // The gate is live (not refusing everything): the re-granted file is
      // allowed and the real rm runs.
      const okGate = await fsGate(worktree, [regrantedFile], DENY_AGENT)
      expect(okGate.decisions[0].allowed).toBe(true)
      if (okGate.decisions[0].allowed) fs.rmSync(okGate.decisions[0].named)
      expect(existsSync(regrantedFile)).toBe(false)
      // And an in-grant file with no later deny is likewise allowed.
      const plainFile = path.join(worktree, "src", "b.rs")
      const plainGate = await fsGate(worktree, [plainFile], DENY_AGENT)
      expect(plainGate.decisions[0].allowed).toBe(true)
      if (plainGate.decisions[0].allowed) fs.rmSync(plainGate.decisions[0].named)
      expect(existsSync(plainFile)).toBe(false)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)

// worktree-write-scope feedback-11: the feedback-10 fix acts on the caller-NAMED
// path, so an op changes the link's OWN location — but only the RESOLVED target
// was scope-checked. A symlink that LIVES in the main checkout and points INTO
// the owned worktree (`<main>/link -> <worktree>/src/a.rs`) resolves in-grant
// and would slip through: `rmSync` would delete the link in main and
// `renameSync` would move the main-located link out of main, mutating the main
// checkout (`global.constraint.worktree-isolation`). The fix checks the link's
// own location too (`canonical parent + basename`) and refuses when EITHER it or
// the resolved target is refused.
test.skipIf(!enabled)(
  "e2e: rm/mv of a MAIN-located symlink into the worktree is refused, link intact",
  async () => {
    const { base, main, worktree } = scratchWorktreeRepo()
    try {
      const target = path.join(worktree, "src", "a.rs")
      const targetBefore = fs.readFileSync(target, "utf8")

      // The link lives in the MAIN checkout; its target resolves into the owned
      // worktree `src/*.rs` grant, so a resolved-only check would allow it.
      const mainLink = path.join(main, "main-link.rs")
      fs.symlinkSync(target, mainLink)

      // rm: refused on the link's OWN location, so the main-located link
      // survives and the worktree target is untouched.
      const rmGate = await fsGate(worktree, [mainLink])
      expect(rmGate.decisions[0].entryAllowed).toBe(false)
      expect(rmGate.decisions[0].entryReason).toBe("path resolves into the main checkout")
      expect(rmGate.decisions[0].allowed).toBe(false)
      expect(rmGate.decisions[0].reason).toBe("path resolves into the main checkout")
      if (rmGate.decisions[0].allowed) fs.rmSync(rmGate.decisions[0].named)
      expect(fs.lstatSync(mainLink).isSymbolicLink()).toBe(true)
      expect(fs.readlinkSync(mainLink)).toBe(target)
      expect(existsSync(target)).toBe(true)

      // mv: the main-located link must not be renamed into the worktree.
      const mvTo = path.join(worktree, "src", "moved.rs")
      const mvGate = await fsGate(worktree, [mainLink, mvTo])
      expect(mvGate.decisions[0].entryAllowed).toBe(false)
      expect(mvGate.decisions[0].allowed).toBe(false)
      expect(mvGate.decisions[0].reason).toBe("path resolves into the main checkout")
      if (mvGate.decisions[0].allowed && mvGate.decisions[1].allowed) {
        fs.renameSync(mvGate.decisions[0].named, mvGate.decisions[1].named)
      }
      expect(fs.lstatSync(mainLink).isSymbolicLink()).toBe(true)
      expect(fs.readlinkSync(mainLink)).toBe(target)
      expect(existsSync(mvTo)).toBe(false)
      expect(existsSync(target)).toBe(true)

      // cp: the same main-located destination link is refused on its entry, so
      // the copy never replaces the link in main.
      const wtSource = path.join(worktree, "src", "b.rs")
      const cpGate = await fsGate(worktree, [wtSource, mainLink])
      expect(cpGate.decisions[1].entryAllowed).toBe(false)
      expect(cpGate.decisions[1].allowed).toBe(false)
      expect(cpGate.decisions[1].reason).toBe("path resolves into the main checkout")
      if (cpGate.decisions[1].allowed) {
        fs.cpSync(cpGate.decisions[0].named, cpGate.decisions[1].named, { recursive: true })
      }
      expect(fs.lstatSync(mainLink).isSymbolicLink()).toBe(true)
      expect(fs.readlinkSync(mainLink)).toBe(target)
      expect(fs.readFileSync(target, "utf8")).toBe(targetBefore)
    } finally {
      fs.rmSync(base, { recursive: true, force: true })
    }
  },
)


