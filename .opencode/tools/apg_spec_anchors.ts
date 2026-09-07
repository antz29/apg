import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's Anchors edges: requirement and task anchors, each marked pending (target is a Future node = not-yet-built code) or resolved (a real code node).",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"

    const reqAnchors = csvToRows(
      await runCypher(
        context,
        `MATCH (r:Requirement)-[:Anchors]->(t) WHERE r.fqn STARTS WITH ${lit(`${project}/spec.`)} RETURN r.id, t.fqn ORDER BY r.id`,
      ),
    )
    const taskAnchors = csvToRows(
      await runCypher(
        context,
        `MATCH (t:Task)-[:Anchors]->(x) WHERE t.fqn STARTS WITH ${lit(`${project}/plan.`)} RETURN t.fqn, x.fqn ORDER BY t.fqn`,
      ),
    )
    // Pending = the anchor target is a Future node (a `<project>/<name>` fqn);
    // resolved = a real code FQN (the `future/` prefix is gone — PHASE_04).
    const futureSet = new Set(
      csvToRows(await runCypher(context, "MATCH (f:Future) RETURN f.fqn"))
        .slice(1)
        .map((r) => r[0]),
    )

    const lines = ["kind,from,target,state"]
    for (const [id, target] of reqAnchors.slice(1)) {
      const state = futureSet.has(target) ? "pending" : "resolved"
      lines.push(`requirement,${id},${target},${state}`)
    }
    for (const [from, target] of taskAnchors.slice(1)) {
      const state = futureSet.has(target) ? "pending" : "resolved"
      lines.push(`task,${from.replace(`${project}/`, "")},${target},${state}`)
    }
    if (lines.length === 1) return `No anchors for spec \`${project}\`.`
    return lines.join("\n")
  },
})