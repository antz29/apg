import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's Anchors edges: requirement and task anchors, each marked pending (target is a future/<...> node = not-yet-built code) or resolved (a real code node).",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"

    const reqAnchors = csvToRows(
      await runCypher(
        context,
        `MATCH (r:Requirement)-[:Anchors]->(t) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN r.id, t.fqn ORDER BY r.id`,
      ),
    )
    const taskAnchors = csvToRows(
      await runCypher(
        context,
        `MATCH (t:Task)-[:Anchors]->(x) WHERE t.fqn STARTS WITH ${lit(`future/${project}/plan.`)} RETURN t.fqn, x.fqn ORDER BY t.fqn`,
      ),
    )

    const lines = ["kind,from,target,state"]
    for (const [id, target] of reqAnchors.slice(1)) {
      const state = target.startsWith("future/") ? "pending" : "resolved"
      lines.push(`requirement,${id},${target},${state}`)
    }
    for (const [from, target] of taskAnchors.slice(1)) {
      const state = target.startsWith("future/") ? "pending" : "resolved"
      lines.push(`task,${from.replace(`future/${project}/`, "")},${target},${state}`)
    }
    if (lines.length === 1) return `No anchors for spec \`${project}\`.`
    return lines.join("\n")
  },
})