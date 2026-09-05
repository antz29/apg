import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows, projectOf } from "../lib/apg.ts"

export default tool({
  description:
    "Spec overview: every spec project with its title, goal, and counts (requirements, phases, futures, notes, feedback, delivered requirements). Use to see which specs exist and their implementation state.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Restrict to one spec project, e.g. workitem-timer (default: all)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE s.fqn = ${lit(`future/${args.project}/spec`)}` : ""
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
        `  requirements: ${reqs} (${delivered} delivered, ${reqs - delivered} planned)  phases: ${count(phRows, p)}  futures: ${count(futRows, p)}  notes: ${count(noteRows, p)}  feedback open: ${feedbackOpen}`,
      )
    }
    return out.join("\n")
  },
})