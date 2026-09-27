import { tool } from "@opencode-ai/plugin"
import {
  runCypher,
  lit,
  csvToRows,
  branchAddedRequirementNames,
  scopeProjectRequirements,
  REQUIREMENT_FQN_PREFIX,
  type RequirementScope,
} from "../lib/apg.ts"

export default tool({
  description:
    "Plan-phase health for a project: unsatisfied requirements (declared but no PlanPhase Satisfies them), requirements Satisfied by more than one phase (violating the exactly-one-phase rule), Gates cycles (a phase transitively gated on itself), phases with no tasks, and tasks under review (done but with unresolved Feedback on the phase or its tasks).",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    project: tool.schema.string().describe("Plan project (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const pfx = `${project}/plan.phase-`

    // Every parse below runs through the guarded boundary: a `runCypher`
    // error string makes `csvToRows` throw (see expectQueryOk), so the whole
    // parse block is wrapped and the verbatim `apg query failed …` message is
    // returned as the tool result instead of crashing with an opaque
    // exception. Containers consumed after the block are declared here.
    let phases: string[][]
    let scope: RequirementScope
    let gates: Array<[string, string]>
    const taskCount = new Map<string, number>()
    const doneSet = new Set<string>()
    const feedbackUnderReview = new Set<string>()
    try {
      phases = csvToRows(
        await runCypher(context, `MATCH (pp:PlanPhase) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, pp.number, pp.title ORDER BY pp.number`, args.directory),
      )
      if (phases.length <= 1) return `No plan for \`${project}\`.`

      // Requirements live in the layers store (global FQNs
      // `requirements.requirement.<name>`, no project prefix); the plan's
      // Satisfies edges point at them by FQN. The full set only ORDERS the
      // scoped findings and is unioned with the scope, so a requirement the
      // scope does not cover is never emitted.
      const allReqs = csvToRows(
        await runCypher(context, `MATCH (r:Requirement) RETURN r.fqn`, args.directory),
      )
        .slice(1)
        .map((r) => r[0])

      // THIS project's Satisfies relation only — count, not membership: the
      // spec requires every requirement Satisfied by EXACTLY one phase of the
      // project, so >1 is a finding too (not just 0). Filtering to `pfx`
      // keeps another project's transient Satisfies edge out of both the scope
      // and the exactly-one-phase rule.
      const satBy = new Map<string, string[]>()
      for (const [r, p] of csvToRows(
        await runCypher(context, `MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN r.fqn, pp.fqn`, args.directory),
      ).slice(1)) {
        satBy.set(r, [...(satBy.get(r) ?? []), p])
      }

      // The scope DECISION is the shared pure core; the git branch delta that
      // feeds it is the shared subprocess wrapper (real I/O, e2e-covered).
      const branchAdded = await branchAddedRequirementNames(context, args.directory)
      scope = scopeProjectRequirements(allReqs, branchAdded, satBy)

      gates = csvToRows(
        await runCypher(context, `MATCH (a:PlanPhase)-[:Gates]->(b:PlanPhase) WHERE a.fqn STARTS WITH ${lit(pfx)} RETURN a.fqn, b.fqn`, args.directory),
      ).slice(1) as Array<[string, string]>
      for (const [phase, , status] of csvToRows(
        await runCypher(context, `MATCH (pp:PlanPhase)-[:Contains]->(t:Task) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, t.fqn, t.status`, args.directory),
      ).slice(1)) {
        taskCount.set(phase, (taskCount.get(phase) ?? 0) + 1)
        if (status === "done") doneSet.add(phase)
      }
      for (const [, status, , target] of csvToRows(
        await runCypher(context, "MATCH (f:Feedback)-[:Reviews]->(n) RETURN f.fqn, f.status, f.disposition, n.fqn", args.directory),
      ).slice(1)) {
        if (status !== "resolved" && target.startsWith(pfx)) feedbackUnderReview.add(target)
      }
    } catch (e) {
      // Surface the verbatim `apg query failed …` message instead of an
      // opaque crash (or a benign "no plan") when the guard rejects a result.
      return e instanceof Error ? e.message : String(e)
    }

    const lines: string[] = []
    for (const [pfqn, number, title] of phases.slice(1)) {
      lines.push(`Phase ${number} — ${title} (${pfqn})`)
      if (!(taskCount.get(pfqn) ?? 0)) lines.push(`  !! no tasks`)
      if (doneSet.has(pfqn) && feedbackUnderReview.has(pfqn)) lines.push(`  !! done but under review (unresolved feedback)`)
    }

    const cycle = detectCycle(gates)
    if (cycle) lines.push(`!! Gates cycle detected: ${cycle.join(" -> ")}`)

    // Findings are scoped to THIS project: `scope` is the branch-added ∪
    // this-project's-Satisfied set, so a requirement delivered by an earlier
    // project (its transient Satisfies edge gone with its branch) is not
    // reported unsatisfied here.
    const reqName = (fqn: string) => fqn.replace(REQUIREMENT_FQN_PREFIX, "")
    if (scope.unsatisfied.length) {
      lines.push(`!! unsatisfied requirements (no PlanPhase Satisfies them): ${scope.unsatisfied.map(reqName).join(", ")}`)
    }
    if (scope.overSatisfied.length) {
      for (const { requirement, phases: projectPhases } of scope.overSatisfied) {
        const phases = projectPhases.map((p) => p.replace(pfx, "")).join(", ")
        lines.push(`!! ${reqName(requirement)} Satisfied by more than one phase (${phases}) — every requirement must be Satisfied by exactly one phase`)
      }
    }
    return lines.join("\n")
  },
})

function detectCycle(edges: Array<[string, string]>): string[] | null {
  const adj = new Map<string, string[]>()
  for (const [a, b] of edges) adj.set(a, [...(adj.get(a) ?? []), b])
  const visiting = new Set<string>()
  const done = new Set<string>()
  const stack: string[] = []
  const visit = (n: string): string[] | null => {
    if (done.has(n)) return null
    if (visiting.has(n)) {
      const i = stack.indexOf(n)
      return [...stack.slice(i), n]
    }
    visiting.add(n)
    stack.push(n)
    for (const m of adj.get(n) ?? []) {
      const c = visit(m)
      if (c) return c
    }
    stack.pop()
    visiting.delete(n)
    done.add(n)
    return null
  }
  for (const [a] of edges) {
    const c = visit(a)
    if (c) return c
  }
  return null
}