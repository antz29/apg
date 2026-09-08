import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's Anchors edges: requirement and task anchors, each marked pending (target is a planned Implementation node or a proposed Solution node = not-yet-built code) or resolved (a real code node).",
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
    // Pending = the anchor target is a planned Implementation node
    // (status: planned) or a proposed Solution node (System/Container/
    // Component); resolved = a real code node. The placeholder node is gone
    // (PHASE_02).
    const pendingSet = new Set<string>()
    for (const label of ["Struct", "Function", "File", "Module"]) {
      for (const r of csvToRows(
        await runCypher(context, `MATCH (n:${label}) WHERE n.status = 'planned' RETURN n.fqn`),
      )) {
        pendingSet.add(r[0])
      }
    }
    for (const label of ["System", "Container", "Component"]) {
      for (const r of csvToRows(await runCypher(context, `MATCH (n:${label}) RETURN n.fqn`))) {
        pendingSet.add(r[0])
      }
    }

    const lines = ["kind,from,target,state"]
    for (const [id, target] of reqAnchors.slice(1)) {
      const state = pendingSet.has(target) ? "pending" : "resolved"
      lines.push(`requirement,${id},${target},${state}`)
    }
    for (const [from, target] of taskAnchors.slice(1)) {
      const state = pendingSet.has(target) ? "pending" : "resolved"
      lines.push(`task,${from.replace(`${project}/`, "")},${target},${state}`)
    }
    if (lines.length === 1) return `No anchors for spec \`${project}\`.`
    return lines.join("\n")
  },
})