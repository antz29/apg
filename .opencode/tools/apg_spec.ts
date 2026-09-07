import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows, projectOf } from "../lib/apg.ts"

export default tool({
  description:
    "Spec overview: every spec project with its title, goal, and counts (requirements, phases, futures, tier nodes, notes, feedback, delivered requirements). Use to see which specs exist and their implementation state.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Restrict to one spec project, e.g. workitem-timer (default: all)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE s.fqn = ${lit(`${args.project}/spec`)}` : ""
    const specs = csvToRows(
      await runCypher(context, `MATCH (s:Spec)${where} RETURN s.fqn, s.title, s.goal ORDER BY s.fqn`),
    )
    if (specs.length <= 1) {
      return "No specs found. Author one with `apg spec init <project> --title ... --goal ...` (or the apg_spec_init tool)."
    }

    const reqRows = csvToRows(await runCypher(context, "MATCH (r:Requirement) RETURN r.fqn"))
    const phRows = csvToRows(await runCypher(context, "MATCH (p:Phase) RETURN p.fqn"))
    const futRows = csvToRows(await runCypher(context, "MATCH (f:Future) RETURN f.fqn"))
    const noteRows = csvToRows(await runCypher(context, "MATCH (n:Note) RETURN n.fqn"))
    const fbRows = csvToRows(await runCypher(context, "MATCH (f:Feedback) RETURN f.fqn, f.status"))
    const implRows = csvToRows(await runCypher(context, "MATCH (c)-[:Implements]->(r:Requirement) RETURN r.fqn"))
    const tierRows = csvToRows(
      await runCypher(
        context,
        `MATCH (n) WHERE n.fqn CONTAINS '/stakeholder.' OR n.fqn CONTAINS '/domain.' OR n.fqn CONTAINS '/subdomain.' OR n.fqn CONTAINS '/entity.' OR n.fqn CONTAINS '/value-object.' OR n.fqn CONTAINS '/aggregate.' OR n.fqn CONTAINS '/domain-event.' OR n.fqn CONTAINS '/domain-process.' OR n.fqn CONTAINS '/domain-rule.' OR n.fqn CONTAINS '/actor.' OR n.fqn CONTAINS '/system.' OR n.fqn CONTAINS '/container.' OR n.fqn CONTAINS '/component.' RETURN n.fqn`,
      ),
    )

    const count = (rows: string[][], p: string) => rows.filter((r) => projectOf(r[0]) === p).length

    const out: string[] = []
    for (const [fqn, title, goal] of specs) {
      const p = projectOf(fqn) ?? fqn
      const reqs = count(reqRows, p)
      const delivered = count(implRows, p)
      const feedbackOpen = fbRows.filter((r) => projectOf(r[0]) === p && r[1] !== "resolved").length
      out.push(`${fqn}\t${title}`)
      if (goal) out.push(`  goal: ${goal}`)
      out.push(
        `  requirements: ${reqs} (${delivered} delivered, ${reqs - delivered} planned)  phases: ${count(phRows, p)}  futures: ${count(futRows, p)}  tier nodes: ${count(tierRows, p)}  notes: ${count(noteRows, p)}  feedback open: ${feedbackOpen}`,
      )
    }
    return out.join("\n")
  },
})