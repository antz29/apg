import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "Plan-phase health for a project: unsatisfied requirements (declared but no PlanPhase Satisfies them), Gates cycles (a phase transitively gated on itself), phases with no tasks, and tasks under review (done but with unresolved Feedback on the phase or its tasks).",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const pfx = `future/${project}/plan.phase-`

    const phases = csvToRows(
      await runCypher(context, `MATCH (pp:PlanPhase) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, pp.number, pp.title ORDER BY pp.number`),
    )
    if (phases.length <= 1) return `No plan for \`${project}\`.`

    const reqs = csvToRows(
      await runCypher(context, `MATCH (r:Requirement) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN r.fqn, r.id`),
    )
    const sat = new Set(
      csvToRows(await runCypher(context, "MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN r.fqn")).map((r) => r[0]),
    )
    const gates: Array<[string, string]> = csvToRows(
      await runCypher(context, `MATCH (a:PlanPhase)-[:Gates]->(b:PlanPhase) WHERE a.fqn STARTS WITH ${lit(pfx)} RETURN a.fqn, b.fqn`),
    ).slice(1) as Array<[string, string]>
    const taskCount = new Map<string, number>()
    const doneSet = new Set<string>()
    for (const [phase, , status] of csvToRows(
      await runCypher(context, `MATCH (pp:PlanPhase)-[:Contains]->(t:Task) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, t.fqn, t.status`),
    ).slice(1)) {
      taskCount.set(phase, (taskCount.get(phase) ?? 0) + 1)
      if (status === "done") doneSet.add(phase)
    }
    const feedbackUnderReview = new Set<string>()
    for (const [, , , target] of csvToRows(
      await runCypher(context, "MATCH (f:Feedback)-[:Reviews]->(n) RETURN f.fqn, f.status, f.disposition, n.fqn"),
    ).slice(1)) {
      if (target.startsWith(pfx)) feedbackUnderReview.add(target)
    }

    const lines: string[] = []
    for (const [pfqn, number, title] of phases.slice(1)) {
      lines.push(`Phase ${number} — ${title} (${pfqn})`)
      if (!(taskCount.get(pfqn) ?? 0)) lines.push(`  !! no tasks`)
      if (doneSet.has(pfqn) && feedbackUnderReview.has(pfqn)) lines.push(`  !! done but under review (unresolved feedback)`)
    }

    const cycle = detectCycle(gates)
    if (cycle) lines.push(`!! Gates cycle detected: ${cycle.join(" -> ")}`)

    const unsatisfied = reqs.slice(1).filter((r) => !sat.has(r[0]))
    if (unsatisfied.length) {
      lines.push(`!! unsatisfied requirements (no PlanPhase Satisfies them): ${unsatisfied.map((r) => r[1]).join(", ")}`)
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