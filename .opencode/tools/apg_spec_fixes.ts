import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List every materialization-fix Note — the record of what the spec-writer changed when materializing a source spec into the graph. Each row: note fqn, the affected target, and the body (source statement, inconsistency, resolution, autonomous/user). The audit trail for the navigator and reviewers.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Restrict to one spec project (default: all projects)."),
  },
  async execute(args, context) {
    const where = args.project
      ? ` WHERE t.fqn STARTS WITH ${lit(`future/${args.project}/`)}`
      : ""
    const rows = csvToRows(
      await runCypher(
        context,
        `MATCH (n:Note)-[:Details]->(t) WHERE n.kind = 'materialization-fix'${where} RETURN n.fqn, n.body, t.fqn ORDER BY n.fqn`,
      ),
    )
    if (rows.length <= 1) {
      return args.project
        ? `No materialization-fix notes for \`${args.project}\`.`
        : "No materialization-fix notes."
    }
    const lines = ["note,target,body"]
    for (const [fqn, body, target] of rows.slice(1)) {
      lines.push(`${fqn},${target},"${body.replace(/"/g, '""')}"`)
    }
    return lines.join("\n")
  },
})