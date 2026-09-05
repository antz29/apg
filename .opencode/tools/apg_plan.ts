import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "Plan overview: each plan project with its strategy, phase table (number, title, deliverable, satisfied requirements), and per-phase task progress (done/total). Plans are transient — they live under apg/.trans/plans/<project>.jsonl.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Restrict to one plan project (default: all)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE p.fqn = ${lit(`future/${args.project}/plan`)}` : ""
    const plans = csvToRows(await runCypher(context, `MATCH (p:Plan)${where} RETURN p.fqn, p.title, p.strategy ORDER BY p.fqn`))
    if (plans.length <= 1) {
      return "No plans found. Author one with `apg plan init <project> --strategy ...` (or the apg_plan_init tool)."
    }

    const phases = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase) RETURN pp.fqn, pp.number, pp.title, pp.deliverable"))
    const sat = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN pp.fqn, r.id"))
    const tasks = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase)-[:Contains]->(t:Task) RETURN pp.fqn, t.fqn, t.status"))
    const builds = csvToRows(await runCypher(context, "MATCH (t:Task)-[:Builds]->(f:Future) RETURN t.fqn, f.fqn"))

    const out: string[] = []
    for (const [planFqn, title, strategy] of plans.slice(1)) {
      const project = planFqn.replace("/plan", "").replace("future/", "")
      out.push(`## ${planFqn}\t${title}`)
      if (strategy) out.push(`  strategy: ${strategy.replace(/\n/g, " ")}`)
      for (const [pfqn, number, ptitle, deliverable] of phases.slice(1)) {
        if (!pfqn.startsWith(`future/${project}/plan.phase-`)) continue
        const satReqs = sat.filter((r) => r[0] === pfqn).map((r) => r[1]).join(",")
        const phaseTasks = tasks.filter((r) => r[0] === pfqn)
        const done = phaseTasks.filter((r) => r[2] === "done").length
        const built = phaseTasks
          .map((r) => builds.find((b) => b[0] === r[1])?.[1])
          .filter(Boolean)
          .join(",")
        out.push(
          `  Phase ${number} — ${ptitle}${deliverable ? ` (${deliverable})` : ""} | satisfies: ${satReqs || "-"} | tasks: ${done}/${phaseTasks.length} done | builds: ${built || "-"}`,
        )
      }
    }
    return out.join("\n")
  },
})