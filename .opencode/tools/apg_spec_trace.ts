import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "Trace one requirement (or every requirement) through the graph: its DependsOn dependencies, its Anchors, and the code that Implements it. The end-to-end 'requirement → deps → anchors → code' view.",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
    reqId: tool.schema
      .string()
      .optional()
      .describe("Requirement id (e.g. R1) to trace; default: all requirements."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"

    const reqs = csvToRows(
      await runCypher(
        context,
        `MATCH (r:Requirement) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN r.fqn, r.id, r.title, r.feature ORDER BY r.fqn`,
      ),
    )
    if (reqs.length <= 1) return `No requirements for spec \`${project}\`.`

    const deps = new Map<string, string[]>()
    for (const [from, to] of csvToRows(
      await runCypher(
        context,
        `MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) WHERE a.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN a.id, b.id`,
      ),
    ).slice(1)) {
      deps.set(from, [...(deps.get(from) ?? []), to])
    }
    const anchors = new Map<string, string[]>()
    for (const [from, to] of csvToRows(
      await runCypher(
        context,
        `MATCH (r:Requirement)-[:Anchors]->(t) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN r.id, t.fqn`,
      ),
    ).slice(1)) {
      anchors.set(from, [...(anchors.get(from) ?? []), to])
    }
    const implements_: Map<string, string[]> = new Map()
    for (const [code, req] of csvToRows(
      await runCypher(
        context,
        `MATCH (c)-[:Implements]->(r:Requirement) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN c.fqn, r.id`,
      ),
    ).slice(1)) {
      implements_.set(req, [...(implements_.get(req) ?? []), code])
    }

    const lines: string[] = []
    for (const [fqn, id, title, feature] of reqs.slice(1)) {
      if (args.reqId && id !== args.reqId) continue
      lines.push(`## ${id} [${feature || "no-feature"}] — ${title}`)
      lines.push(`  fqn: ${fqn}`)
      const d = deps.get(id) ?? []
      lines.push(`  consumes: ${d.length ? d.join(", ") : "(none)"}`)
      const a = anchors.get(id) ?? []
      lines.push(`  anchors: ${a.length ? a.map((x) => (x.startsWith("future/") ? `${x} (pending)` : x)).join(", ") : "(none)"}`)
      const i = implements_.get(id) ?? []
      lines.push(`  implemented by: ${i.length ? i.join(", ") : "(none — planned)"}`)
    }
    if (lines.length === 0) return `Requirement \`${args.reqId}\` not found in spec \`${project}\`.`
    return lines.join("\n")
  },
})