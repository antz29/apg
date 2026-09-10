import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "Plan overview: each plan project with its strategy, phase table (number, title, deliverable, satisfied requirements), and per-phase task progress (done/total). Plans are transient — they live under apg/.trans/plans/<project>.jsonl and never commit.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    project: tool.schema
      .string()
      .optional()
      .describe("Restrict to one plan project (default: all)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE p.fqn = ${lit(`${args.project}/plan`)}` : ""
    const plans = csvToRows(await runCypher(context, `MATCH (p:Plan)${where} RETURN p.fqn, p.title, p.strategy ORDER BY p.fqn`, args.directory))
    if (plans.length <= 1) {
      return "No plans found. Author one with `apg plan init <project> --strategy ...` (or the apg_plan_init tool)."
    }

    const phases = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase) RETURN pp.fqn, pp.number, pp.title, pp.deliverable", args.directory))
    const sat = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN pp.fqn, r.fqn", args.directory))
    const tasks = csvToRows(await runCypher(context, "MATCH (pp:PlanPhase)-[:Contains]->(t:Task) RETURN pp.fqn, t.fqn, t.status", args.directory))
    const verbs = csvToRows(await runCypher(context, "MATCH (t:Task) RETURN t.fqn, t.verb, t.target", args.directory))

    const out: string[] = []
    for (const [planFqn, title, strategy] of plans.slice(1)) {
      const project = planFqn.replace("/plan", "")
      out.push(`## ${planFqn}\t${title}`)
      if (strategy) out.push(`  strategy: ${strategy.replace(/\n/g, " ")}`)
      for (const [pfqn, number, ptitle, deliverable] of phases.slice(1)) {
        if (!pfqn.startsWith(`${project}/plan.phase-`)) continue
        const satReqs = sat
          .filter((r) => r[0] === pfqn)
          .map((r) => r[1].replace("requirements.requirement.", ""))
          .join(",")
        const phaseTasks = tasks.filter((r) => r[0] === pfqn)
        const done = phaseTasks.filter((r) => r[2] === "done").length
        const targets = phaseTasks
          .map((r) => {
            const v = verbs.find((x) => x[0] === r[1])
            return v && v[2] ? `${v[1]} ${v[2]}` : ""
          })
          .filter(Boolean)
          .join(",")
        out.push(
          `  Phase ${number} — ${ptitle}${deliverable ? ` (${deliverable})` : ""} | satisfies: ${satReqs || "-"} | tasks: ${done}/${phaseTasks.length} done | targets: ${targets || "-"}`,
        )
      }
    }
    return out.join("\n")
  },
})
