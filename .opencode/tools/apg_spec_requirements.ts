import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's requirements with id, feature, derived status (delivered = an Implements edge exists, else planned), title, and body. Filter by feature or status.",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
    feature: tool.schema.string().optional().describe("Only requirements grouped under this feature."),
    status: tool.schema
      .string()
      .optional()
      .describe('Only requirements in this derived state: "planned" or "delivered".'),
    limit: tool.schema.string().optional().describe("Max rows (default 500)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    const reqs = csvToRows(
      await runCypher(
        context,
        `MATCH (r:Requirement) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN r.fqn, r.id, r.title, r.body, r.feature ORDER BY r.fqn LIMIT ${limit}`,
      ),
    )
    if (reqs.length <= 1) return `No requirements for spec \`${project}\`. Add one with \`apg spec add ${project} requirement R1 --title ...\` (or the apg_spec_add tool).`

    const impl = new Set(
      csvToRows(
        await runCypher(
          context,
          `MATCH (c)-[:Implements]->(r:Requirement) WHERE r.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN DISTINCT r.fqn`,
        ),
      ).map((r) => r[0]),
    )

    const rows = reqs.slice(1).map(([fqn, id, title, body, feature]) => {
      const status = impl.has(fqn) ? "delivered" : "planned"
      return [id, feature, status, title, body, fqn]
    })
    const filtered = rows.filter(
      (r) =>
        (!args.feature || r[1] === args.feature) &&
        (!args.status || r[2] === args.status),
    )
    if (filtered.length === 0) return `No requirements match feature=${args.feature ?? "any"} status=${args.status ?? "any"}`

    const head = ["id", "feature", "status", "title", "body", "fqn"]
    const lines = [head.join(",")]
    for (const r of filtered) lines.push(r.map((c) => `"${c.replace(/"/g, '""')}"`).join(","))
    return lines.join("\n")
  },
})