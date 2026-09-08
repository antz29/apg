import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a plan's tasks with phase, kind (owning role), tier (verification depth, test only), status, the planned Implementation node each task Builds, and its Anchors (files/code touched). The implementation checklist view.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    status: tool.schema
      .string()
      .optional()
      .describe('Only tasks in this state: "pending" or "done".'),
    limit: tool.schema.string().optional().describe("Max rows (default 500)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const pfx = `${project}/plan.`
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    const tasks = csvToRows(
      await runCypher(
        context,
        `MATCH (pp:PlanPhase)-[:Contains]->(t:Task) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, t.fqn, t.title, t.kind, t.tier, t.status ORDER BY t.fqn LIMIT ${limit}`,
      ),
    )
    if (tasks.length <= 1) return `No tasks in plan \`${project}\`.`

    const builds = new Map<string, string>()
    for (const [t, f] of csvToRows(
      await runCypher(context, `MATCH (t:Task)-[:Builds]->(p) WHERE t.fqn STARTS WITH ${lit(pfx)} RETURN t.fqn, p.fqn`),
    ).slice(1)) {
      builds.set(t, f)
    }
    const anchors = new Map<string, string[]>()
    for (const [t, x] of csvToRows(
      await runCypher(context, `MATCH (t:Task)-[:Anchors]->(x) WHERE t.fqn STARTS WITH ${lit(pfx)} RETURN t.fqn, x.fqn`),
    ).slice(1)) {
      anchors.set(t, [...(anchors.get(t) ?? []), x])
    }

    const lines = ["task,phase,title,kind,tier,status,builds,anchors"]
    for (const [phase, fqn, title, kind, tier, status] of tasks.slice(1)) {
      if (args.status && status !== args.status) continue
      const short = fqn.replace(`${project}/`, "")
      const phaseShort = phase.replace(`${project}/`, "")
      lines.push(
        [
          short,
          phaseShort,
          `"${title.replace(/"/g, '""')}"`,
          kind,
          tier,
          status,
          (builds.get(fqn) ?? "").replace(`${project}/`, ""),
          (anchors.get(fqn) ?? []).join(";"),
        ].join(","),
      )
    }
    return lines.length > 1 ? lines.join("\n") : `No tasks match status=${args.status ?? "any"}`
  },
})