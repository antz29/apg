import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a plan's tasks with phase, kind (owning role), tier (verification depth, test only), status, the Task→Implementation verb (creates/modifies/deletes/renames/moves) with its target FQN(s), and the new FQN for renames/moves. The implementation checklist view.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
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
        `MATCH (pp:PlanPhase)-[:Contains]->(t:Task) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, t.fqn, t.title, t.kind, t.tier, t.status, t.verb, t.target, t.new_fqn ORDER BY t.fqn LIMIT ${limit}`,
        args.directory,
      ),
    )
    if (tasks.length <= 1) return `No tasks in plan \`${project}\`.`

    const lines = ["task,phase,title,kind,tier,status,verb,target,new_fqn"]
    for (const [phase, fqn, title, kind, tier, status, verb, target, newFqn] of tasks.slice(1)) {
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
          verb,
          target,
          newFqn,
        ].join(","),
      )
    }
    return lines.length > 1 ? lines.join("\n") : `No tasks match status=${args.status ?? "any"}`
  },
})
